//! Offline declaration rendering; never performs retrieval, classification or execution.
use plasm_core::{
    capability_exposure::selected_capability_surface,
    prerequisites::{prerequisite_closure, CapabilityRef, DeploymentBindings},
    prompt_render::{
        python::{prepare_python_teaching_wave, PythonTeachingState},
        render_prerequisite_bindings,
    },
    TeachingExposureSession, CGS,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Deserialize)]
struct Input {
    catalogs: BTreeMap<String, String>,
    bindings: String,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    id: String,
    selected: Vec<CapabilityRef>,
    retrieved: Vec<CapabilityRef>,
}
#[derive(Serialize)]
struct Measurement {
    id: String,
    arm: &'static str,
    candidates: usize,
    closure: plasm_core::prerequisites::PrerequisiteClosure,
    declarations: String,
    prerequisite_bindings: String,
    coverage: BTreeMap<String, Option<String>>,
}

fn render(
    catalogs: &BTreeMap<String, CGS>,
    bindings: &DeploymentBindings,
    business: &[CapabilityRef],
    id: String,
    arm: &'static str,
) -> Result<Measurement, Box<dyn std::error::Error>> {
    let refs = catalogs.iter().map(|(k, v)| (k.clone(), v)).collect();
    let allowed = catalogs.keys().cloned().collect();
    let closure = prerequisite_closure(&refs, bindings, business, &allowed)?;
    let mut exposure = None;
    let layers: Vec<_> = catalogs.values().collect();
    for (entry, cgs) in catalogs {
        let names: Vec<_> = closure
            .business
            .iter()
            .chain(&closure.input_sources)
            .chain(&closure.prerequisites)
            .filter(|r| &r.catalog == entry)
            .map(|r| r.capability.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if names.is_empty() {
            continue;
        }
        let delta = selected_capability_surface(cgs, entry, &names)?;
        let entities: Vec<_> = delta
            .required
            .entities
            .iter()
            .map(|e| e.entity.as_str())
            .collect();
        match &mut exposure {
            None => {
                exposure = Some(TeachingExposureSession::new_with_intent_delta(
                    cgs,
                    entry,
                    &entities,
                    delta.clone(),
                ))
            }
            Some(session) => {
                session.expose_surface(
                    &layers,
                    Arc::new(cgs.clone()),
                    entry,
                    &entities,
                    delta.clone(),
                );
            }
        }
    }
    let (declarations, prerequisite_bindings, coverage) = match exposure {
        None => (String::new(), String::new(), BTreeMap::new()),
        Some(session) => {
            let wave = prepare_python_teaching_wave(&session, &PythonTeachingState::default())?;
            let coverage = wave
                .capabilities
                .into_iter()
                .map(|c| (format!("{}/{}", c.entry_id, c.capability), c.unavailable))
                .collect();
            (
                wave.declarations,
                render_prerequisite_bindings(&closure, &refs, session.to_symbol_map().as_ref())?,
                coverage,
            )
        }
    };
    Ok(Measurement {
        id,
        arm,
        candidates: business.len(),
        closure,
        declarations,
        prerequisite_bindings,
        coverage,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("expected offline input JSON path")?;
    let input: Input = serde_json::from_slice(&std::fs::read(path)?)?;
    let catalogs: BTreeMap<String, CGS> = input
        .catalogs
        .into_iter()
        .map(|(key, path)| Ok((key, serde_json::from_slice(&std::fs::read(path)?)?)))
        .collect::<Result<_, Box<dyn std::error::Error>>>()?;
    let bindings = serde_json::from_slice(&std::fs::read(input.bindings)?)?;
    let mut rows = Vec::new();
    for case in input.cases {
        rows.push(render(
            &catalogs,
            &bindings,
            &case.selected,
            case.id.clone(),
            "classified",
        )?);
        rows.push(render(
            &catalogs,
            &bindings,
            &case.retrieved,
            case.id,
            "all_retrieved",
        )?);
    }
    println!("{}", serde_json::to_string(&rows)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::prerequisites::DeploymentBinding;

    #[test]
    fn retained_candidates_keep_cross_catalog_prerequisites_and_policy_boundary() {
        let graph = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/prerequisite_matrix"),
        )
        .unwrap();
        let read = CapabilityRef {
            catalog: "consumer".into(),
            capability: "read".into(),
        };
        let bindings = DeploymentBindings {
            bindings: vec![DeploymentBinding {
                consumer: read.clone(),
                requirement: "scoped_access".into(),
                provider_catalog: "source".into(),
                provider: "value_source".into(),
            }],
        };
        let catalogs: BTreeMap<String, CGS> =
            BTreeMap::from([("consumer".into(), graph.clone()), ("source".into(), graph)]);
        let refs = catalogs.iter().map(|(k, v)| (k.clone(), v)).collect();
        assert!(prerequisite_closure(
            &refs,
            &bindings,
            std::slice::from_ref(&read),
            &BTreeSet::from(["consumer".into()])
        )
        .is_err());
        let measured = render(
            &catalogs,
            &bindings,
            &[read],
            "federated".into(),
            "all_retrieved",
        )
        .unwrap();
        assert_eq!(
            measured.closure.prerequisites,
            vec![CapabilityRef {
                catalog: "source".into(),
                capability: "acquire".into()
            }]
        );
        assert_eq!(measured.coverage.len(), 2);
        assert!(measured.coverage.values().all(Option::is_none));
        assert!(measured.declarations.contains("credential"));
        assert!(measured.declarations.contains("account"));
        assert_eq!(measured.closure.edges.len(), 1);
    }

    #[test]
    fn retained_candidate_matrix_preserves_closure_and_complete_surface() {
        let cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/prerequisite_matrix"),
        )
        .unwrap();
        let read = CapabilityRef {
            catalog: "matrix".into(),
            capability: "read".into(),
        };
        let bindings = DeploymentBindings {
            bindings: vec![DeploymentBinding {
                consumer: read.clone(),
                requirement: "scoped_access".into(),
                provider_catalog: "matrix".into(),
                provider: "value_source".into(),
            }],
        };
        for count in [1, 16, 64, 128] {
            let mut graph = cgs.clone();
            let template = graph.capabilities["operate"].clone();
            let mut retained = vec![read.clone()];
            for i in 1..count {
                let mut capability = template.clone();
                let name = format!("operation_{i}");
                capability.name = name.clone().into();
                graph.capabilities.insert(name.clone().into(), capability);
                retained.push(CapabilityRef {
                    catalog: "matrix".into(),
                    capability: name,
                });
            }
            let before = serde_json::to_value(&graph).unwrap();
            let catalogs = BTreeMap::from([("matrix".into(), graph)]);
            let measured = render(
                &catalogs,
                &bindings,
                &retained,
                "matrix".into(),
                "all_retrieved",
            )
            .unwrap();
            assert_eq!(measured.closure.business.len(), count);
            assert_eq!(measured.closure.prerequisites.len(), 1);
            assert_eq!(measured.closure.prerequisites[0].capability, "acquire");
            assert!(!measured.closure.edges.is_empty());
            assert_eq!(measured.coverage.len(), count + 1);
            assert!(measured.coverage.values().all(Option::is_none));
            let names: Vec<_> = retained
                .iter()
                .map(|r| r.capability.clone())
                .chain(["acquire".into()])
                .collect();
            let all = selected_capability_surface(&catalogs["matrix"], "matrix", &names).unwrap();
            for name in &names {
                let one = selected_capability_surface(
                    &catalogs["matrix"],
                    "matrix",
                    std::slice::from_ref(name),
                )
                .unwrap();
                assert!(one
                    .required
                    .capabilities
                    .is_subset(&all.required.capabilities));
                assert!(one.required.slots.is_subset(&all.required.slots));
                assert!(one.required.entities.is_subset(&all.required.entities));
            }
            assert!(
                measured
                    .declarations
                    .contains("Changes the selected business record")
                    || count == 1
            );
            assert!(measured.declarations.contains("credential"));
            assert!(measured.declarations.contains("account"));
            assert_eq!(before, serde_json::to_value(&catalogs["matrix"]).unwrap());
            // Every rendered method is still a declaration; no backend is instantiated.
        }
    }
}
