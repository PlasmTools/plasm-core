//! OPH-1 / OPH-2: concurrent hydrate under barrier release schedules.
//!
//! Replaces sleep-based reordering. Asserts multiple GETs were in flight before release.
//!
//! Residual: correct-identity + wrong-fields faults are generally undetectable; a dedicated
//! probe documents that gap rather than claiming Plasm rejects it.

use super::*;
use crate::cache::{CachedEntity, EntityCompleteness};
use crate::execution::{
    ExecuteSessionMaterial, ExecutionConfig, ExecutionEngine, ExecutionMode, SessionMaterialization,
};
use plasm_compile::CmlEnv;
use plasm_core::loader::load_schema_dir;
use plasm_core::{Ref, Value};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use std::collections::BTreeMap;
use std::sync::Arc;

fn load_lang_cgs() -> plasm_core::CGS {
    load_schema_dir(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix"),
    )
    .expect("language matrix CGS")
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio rt")
        .block_on(f)
}

fn env_with_token() -> CmlEnv {
    let mut env = CmlEnv::new();
    env.insert("access_token".into(), Value::String("tok".into()));
    env
}

#[derive(Debug, Clone)]
struct NoteSeed {
    id: String,
    title: String,
    body: String,
}

fn published_fields(out: &[CachedEntity]) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for e in out {
        let id = e.reference.primary_slot_str().to_string();
        let mut fields = BTreeMap::new();
        if let Some(Value::String(s)) = e.fields.get("title").map(|f| f.to_value()) {
            fields.insert("title".into(), s);
        }
        if let Some(Value::String(s)) = e.fields.get("body").map(|f| f.to_value()) {
            fields.insert("body".into(), s);
        }
        map.insert(id, fields);
    }
    map
}

/// Run concurrent hydrate with an explicit release order; return published rows + harness.
fn run_hydrate_wave(
    seeds: &[NoteSeed],
    release_order: &[String],
    faults: BTreeMap<String, ResponseFault>,
    injected: BTreeMap<String, BTreeMap<String, String>>,
) -> Result<
    (
        Vec<CachedEntity>,
        HonestyHarness,
        usize,
        SessionMaterialization,
    ),
    String,
> {
    let schedule = BarrierSchedule::new();
    let harness = HonestyHarness::new().with_schedule(schedule.clone());
    *harness.note_faults.lock().expect("faults") = faults;
    *harness.injected_fields.lock().expect("inject") = injected;

    for s in seeds {
        harness.backend_mut(|b| b.seed_note(&s.id, &s.title, &s.body));
    }

    let cgs = load_lang_cgs();
    let compiled =
        Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).map_err(|e| e.to_string())?);
    let material = Arc::new(ExecuteSessionMaterial {
        catalog_revision: "langmatrix".into(),
        compiled_catalog: compiled.clone(),
        credential_store: None,
        prompt_hash: "a".repeat(64),
        session_id: "b".repeat(32),
        transport_origin: None,
        ui_origin: None,
        catalog_bind: None,
        login_access_token_tail: ExecuteSessionMaterial::empty_login_access_token_tail(),
    });

    let concurrency = seeds.len().max(1);
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://127.0.0.1:9".into()),
            hydrate_concurrency: concurrency,
            ..ExecutionConfig::default()
        },
        Arc::new(harness.clone()),
        None,
    );

    let summaries: Vec<CachedEntity> = seeds
        .iter()
        .map(|s| {
            CachedEntity::from_decoded(
                Ref::new("LangSecuredNote", &s.id),
                [
                    ("note_id".into(), Value::String(s.id.clone())),
                    ("title".into(), Value::String(format!("list-{}", s.title))),
                ]
                .into_iter()
                .collect(),
                indexmap::IndexMap::new(),
                1,
                EntityCompleteness::Summary,
            )
        })
        .collect();

    let env = env_with_token();
    let base: Arc<str> = Arc::from("http://127.0.0.1:9");
    let release_order = release_order.to_vec();
    let schedule_driver = schedule.clone();

    let out = block_on(ExecutionEngine::run_in_execute_task_scopes(
        base,
        None,
        None,
        None,
        Some(material),
        compiled,
        None,
        None,
        async move {
            let mut mat = SessionMaterialization::new();
            for summary in &summaries {
                mat.insert(summary.clone()).expect("seed cached summary");
            }
            let hydrate = engine.hydrate_query_summaries(
                "LangSecuredNote",
                &summaries,
                &cgs,
                &mut mat,
                ExecutionMode::Live,
                true,
                &env,
            );
            let release = async {
                run_release_wave(&schedule_driver, &release_order).await;
            };
            let (hydrate_res, _) = tokio::join!(hydrate, release);
            let (out, _) = hydrate_res.map_err(|e| e.to_string())?;
            Ok::<_, String>((out, schedule_driver.max_in_flight(), mat))
        },
    ))?;

    let (entities, max_in_flight, mat) = out;
    let recorded_order = harness.with_model(|model| {
        model
            .responses
            .iter()
            .map(|response| response.request_id.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        recorded_order,
        schedule
            .completion_order()
            .iter()
            .map(|id| format!("get:{id}"))
            .collect::<Vec<_>>(),
        "transport records must witness the handshake order"
    );
    Ok((entities, harness, max_in_flight, mat))
}

fn assert_order_invariant_against_backend(
    seeds: &[NoteSeed],
    out: &[CachedEntity],
    faults: &BTreeMap<String, ResponseFault>,
) -> Result<(), String> {
    for s in seeds {
        if faults.get(&s.id) == Some(&ResponseFault::WrongIdentity) {
            // Retained as summary; must not carry injected membership as Complete.
            let row = out
                .iter()
                .find(|e| e.reference.primary_slot_str() == s.id)
                .ok_or_else(|| format!("missing row {}", s.id))?;
            if row.completeness == EntityCompleteness::Complete
                && row.fields.get("body").map(|f| f.to_value())
                    != Some(Value::String(s.body.clone()))
            {
                // Complete with non-backend body would be contamination; Summary is ok.
                return Err(format!(
                    "OPH-1/2: wrong-identity row {} upgraded dishonestly",
                    s.id
                ));
            }
            continue;
        }
        if faults.get(&s.id) == Some(&ResponseFault::WrongFields) {
            // Undetectable: Plasm may accept lying fields. Skip equality vs backend.
            continue;
        }
        let row = out
            .iter()
            .find(|e| e.reference.primary_slot_str() == s.id)
            .ok_or_else(|| format!("missing published row {}", s.id))?;
        let title = row.fields.get("title").map(|f| f.to_value());
        let body = row.fields.get("body").map(|f| f.to_value());
        if title != Some(Value::String(s.title.clone()))
            || body != Some(Value::String(s.body.clone()))
        {
            return Err(format!(
                "OPH-2 order invariance: row {} got title={title:?} body={body:?}, \
                 expected title={} body={}",
                s.id, s.title, s.body
            ));
        }
        if row.completeness != EntityCompleteness::Complete {
            return Err(format!(
                "OPH-2: well-behaved hydrate for {} should be Complete",
                s.id
            ));
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// OPH-2: every permutation of completion order yields the same semantic publish
    /// for a fixed backend snapshot (distinct identities, well-behaved responses).
    #[test]
    fn oph_hydrate_order_invariance(
        seed_n in 2usize..=4,
        perm_seed in 0u64..10_000,
    ) {
        let seeds: Vec<NoteSeed> = (0..seed_n)
            .map(|i| NoteSeed {
                id: format!("{}", i + 1),
                title: format!("code-{i}"),
                body: format!("member-{i}"),
            })
            .collect();
        let mut order: Vec<String> = seeds.iter().map(|s| s.id.clone()).collect();
        // Deterministic shuffle from perm_seed.
        {
            let mut state = perm_seed;
            for i in (1..order.len()).rev() {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1);
                let j = (state as usize) % (i + 1);
                order.swap(i, j);
            }
        }
        let faults = BTreeMap::new();
        let (out, harness, max_in_flight, _mat) =
            run_hydrate_wave(&seeds, &order, faults.clone(), BTreeMap::new())
                .expect("hydrate wave");
        prop_assert!(
            max_in_flight >= seeds.len(),
            "expected all {} GETs in flight before release, got {max_in_flight}",
            seeds.len()
        );
        let check = assert_order_invariant_against_backend(&seeds, &out, &faults);
        prop_assert!(check.is_ok(), "order={order:?}: {}", check.unwrap_err());

        // Identity isolation vacuously ok with no wrong-identity faults.
        let published = published_fields(&out);
        let responses = harness.with_model(|m| m.responses.clone());
        let iso = ReferenceModel::check_identity_isolation(&published, &responses);
        prop_assert!(iso.is_ok(), "{}", iso.unwrap_err());
    }

    /// OPH-1: wrong-identity inject under concurrent schedule does not contaminate neighbors.
    #[test]
    fn oph_hydrate_wrong_identity_isolation(
        victim in 0usize..3,
        perm_seed in 0u64..10_000,
    ) {
        let seeds: Vec<NoteSeed> = (0..3)
            .map(|i| NoteSeed {
                id: format!("{}", i + 1),
                title: format!("code-{i}"),
                body: format!("member-{i}"),
            })
            .collect();
        let victim_id = format!("{}", (victim % 3) + 1);
        let mut order: Vec<String> = seeds.iter().map(|s| s.id.clone()).collect();
        {
            let mut state = perm_seed;
            for i in (1..order.len()).rev() {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let j = (state as usize) % (i + 1);
                order.swap(i, j);
            }
        }
        let mut faults = BTreeMap::new();
        faults.insert(victim_id.clone(), ResponseFault::WrongIdentity);
        let mut injected = BTreeMap::new();
        let mut inj_fields = BTreeMap::new();
        inj_fields.insert("note_id".into(), "99".into());
        inj_fields.insert("title".into(), "code-injected".into());
        inj_fields.insert("body".into(), "member-injected".into());
        injected.insert(victim_id.clone(), inj_fields);

        let (out, harness, max_in_flight, mat) =
            run_hydrate_wave(&seeds, &order, faults.clone(), injected)
                .expect("hydrate wave");
        prop_assert!(max_in_flight >= 3);

        let published = published_fields(&out);
        let responses = harness.with_model(|m| m.responses.clone());
        let iso = ReferenceModel::check_identity_isolation(&published, &responses);
        prop_assert!(iso.is_ok(), "victim={victim_id} order={order:?}: {}", iso.unwrap_err());

        prop_assert!(mat.get(&Ref::new("LangSecuredNote", "99")).is_none(), "foreign identity cached");
        let retained = out.iter().find(|e| e.reference == Ref::new("LangSecuredNote", &victim_id)).expect("retained summary");
        prop_assert_eq!(retained.completeness, EntityCompleteness::Summary);
        let seed = &seeds[victim];
        prop_assert_eq!(retained.fields.get("title").map(|f| f.to_value()), Some(Value::String(format!("list-{}", seed.title))));
        prop_assert!(retained.fields.get("body").is_none());
        let cached = mat.get(&Ref::new("LangSecuredNote", &victim_id)).expect("victim summary remains cached");
        prop_assert_eq!(cached.completeness, EntityCompleteness::Summary);
        prop_assert_eq!(cached.fields.get("title").map(|f| f.to_value()), Some(Value::String(format!("list-{}", seed.title))));
        prop_assert!(cached.fields.get("body").is_none());

        for seed in &seeds {
            if seed.id == victim_id { continue; }
            let cached = mat.get(&Ref::new("LangSecuredNote", &seed.id)).expect("neighbor cached");
            prop_assert_eq!(cached.fields.get("title").map(|f| f.to_value()), Some(Value::String(seed.title.clone())));
            prop_assert_eq!(cached.fields.get("body").map(|f| f.to_value()), Some(Value::String(seed.body.clone())));
        }

        // Injected identity must not appear.
        prop_assert!(
            !published.contains_key("99"),
            "divergent identity 99 was published"
        );

        let neighbors_ok = assert_order_invariant_against_backend(&seeds, &out, &faults);
        prop_assert!(neighbors_ok.is_ok(), "{}", neighbors_ok.unwrap_err());
    }
}

/// Documents residual gap: correct identity + wrong fields is accepted (undetectable).
#[test]
fn residual_wrong_fields_with_matching_identity_are_accepted() {
    let seeds = vec![
        NoteSeed {
            id: "1".into(),
            title: "code-real".into(),
            body: "member-real".into(),
        },
        NoteSeed {
            id: "2".into(),
            title: "code-b".into(),
            body: "member-b".into(),
        },
    ];
    let order = vec!["2".into(), "1".into()];
    let mut faults = BTreeMap::new();
    faults.insert("1".into(), ResponseFault::WrongFields);
    let mut injected = BTreeMap::new();
    let mut inj = BTreeMap::new();
    inj.insert("title".into(), "lie-title".into());
    inj.insert("body".into(), "lie-body".into());
    injected.insert("1".into(), inj);

    let (out, _, max_in_flight, _) =
        run_hydrate_wave(&seeds, &order, faults, injected).expect("wave");
    assert!(max_in_flight >= 2);
    let row1 = out
        .iter()
        .find(|e| e.reference.primary_slot_str() == "1")
        .expect("row 1");
    // Residual gap: Plasm cannot detect field lies when identity matches.
    assert_eq!(
        row1.fields.get("title").map(|f| f.to_value()),
        Some(Value::String("lie-title".into()))
    );
    assert_eq!(row1.completeness, EntityCompleteness::Complete);
}
