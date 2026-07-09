    // DAG compile + dry-run unit tests (no HTTP).
    //
    // Prefer `cargo test -p plasm-e2e --test plasm_language_matrix` for author-visible
    // “this program means X” semantics on the language-matrix fixture. Keep tests here for
    // compiler/plan invariants (splitting, diagnostics, federation quirks, GitHub-shaped
    // graphs). When a case overlaps the matrix, cite the matrix row id on the test (e.g.
    // `lang_domain_symbol_page_size`).
    use super::*;
    use crate::plasm_plan_run::{
        evaluate_plasm_plan_dry, render_plasm_plan_dry_text, symbol_map_for_plasm_surface_parse,
    };
    use plasm_core::{load_schema, CgsContext, PromptPipelineConfig, TeachingExposureSession, CGS};
    use serde_json::json;
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    mod test_support;
    use test_support::github_issue_label_session;

    mod github_symbol_resolution;

    mod homograph_matrix;

    fn test_session() -> ExecuteSession {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("load plasm_language_matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "langmatrix".into(),
            Arc::new(CgsContext::entry("langmatrix", cgs.clone())),
        );
        let exp =
            TeachingExposureSession::new(cgs.as_ref(), "langmatrix", &["LangItem", "LangLine"]);
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "langmatrix".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into(), "LangLine".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    #[test]
    fn search_group_by_rejects_fields_outside_capability_provides() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "search-group-by-relation",
            r#"rows = LangItem~"probe"
bad = rows.group_by(summary)
bad"#,
        )
        .expect_err("search rows omit relation fields from provides");
        assert!(err.contains("not a row field"), "{err}");
        assert!(
            !err.contains("projected columns"),
            "diagnostic must not steer agents toward wire column names: {err}"
        );
    }

    #[test]
    fn multi_postfix_roots_without_binding_errors_with_guidance() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "multi-postfix-roots",
            r#"issue = LangItem("i1")
comments = issue.lines
comments.filter{title="a"}
comments[title]"#,
        )
        .expect_err("intermediate postfix must be bound");
        assert!(
            err.contains("binding") || err.contains("Intermediate"),
            "{err}"
        );
        assert!(!err.contains("return_1"), "{err}");
        assert!(!err.contains("offset"), "{err}");
    }

    #[test]
    fn binding_after_return_line_errors() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "binding-after-return",
            r#"rows = LangItem
rows[title]
lines = rows.lines"#,
        )
        .expect_err("binding after return");
        assert!(err.contains("Return must be last"), "{err}");
    }

    #[test]
    fn flat_projection_on_in_scope_binding_still_compiles() {
        let session = test_session();
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flat-projection-regression",
            "issue = LangItem(\"i1\") comments = issue.lines comments[note]",
        )
        .expect("flat projection return");
        let ret = plan["return"].pointer("/node").and_then(|v| v.as_str());
        assert_eq!(ret, Some("return_1"));
    }

    #[test]
    fn search_group_by_rejects_filter_input_param() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "search-group-by-filter-input",
            r#"rows = LangItem~"probe"{team_key="eng"}
bad = rows.group_by(q)
bad"#,
        )
        .expect_err("search text param q is not a row field");
        assert!(
            err.contains("query/capability input")
                || err.contains("not a row field")
                || err.contains("not a row symbol"),
            "{err}"
        );
        assert!(
            err.contains("not a row field") || err.contains("not a row symbol"),
            "{err}"
        );
    }

    #[test]
    fn search_projection_rejects_filter_input_param() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "search-projection-filter-input",
            r#"rows = LangItem~"probe"{team_key="eng"}[q]
rows"#,
        )
        .expect_err("filter params are inputs not row fields for projection");
        assert!(err.contains("not a row field"), "{err}");
    }

    #[test]
    fn search_group_by_accepts_filter_param_when_in_provides() {
        let session = test_session();
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "search-group-by-filter-in-provides",
            r#"rows = LangItem~"probe"{team_key="eng"}
by_team = rows.group_by(team_key)
by_team"#,
        )
        .expect("team_key in provides should be a valid group_by key");
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert!(!dry.node_results.is_empty());
    }

    #[test]
    fn derive_map_rejects_surface_entity_ctor() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "derive-reject-entity-ctor",
            r#"hits = LangItem
bad = hits => LangItem(id=_.id)
bad"#,
        )
        .expect_err("entity ctor on => must not compile as derive literal");
        assert!(err.contains("derive map does not accept"), "{err}");
    }

    #[test]
    fn derive_map_rejects_bare_relation_arrow_fragment() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "derive-reject-bare-relation-arrow",
            "pika => e2.r3",
        )
        .expect_err("bare source => relation hop must not compile");
        assert!(err.contains("Relation reads use"), "{err}");
    }

    #[test]
    fn derive_map_rejects_relation_hop_symbol() {
        let session = test_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let e_langline = map.entity_sym_for("langmatrix", "LangLine");
        let r_lines = map.ident_sym_relation_for("langmatrix", "LangItem", "lines");
        let program = format!("hits = LangItem\nbad = hits => {e_langline}.{r_lines}\nbad");
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "derive-reject-relation-hop-symbol",
            &program,
        )
        .expect_err("entity relation hop on => must not compile as derive literal");
        assert!(
            err.contains("Relation reads use") || err.contains("derive map does not accept"),
            "{err}"
        );
    }

    #[test]
    fn derive_map_rejects_binding_relation_hop() {
        let session = test_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let r_lines = map.ident_sym_relation_for("langmatrix", "LangItem", "lines");
        let program = format!("hits = LangItem\nbad = hits => hits.{r_lines}\nbad");
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "derive-reject-binding-relation-hop",
            &program,
        )
        .expect_err("binding relation hop on => must not compile as derive");
        assert!(err.contains("Relation reads use"), "{err}");
    }

    #[test]
    fn derive_map_rejects_wire_relation_hop() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "derive-reject-wire-relation-hop",
            r#"hits = LangItem
bad = hits => hits.lines
bad"#,
        )
        .expect_err("wire relation hop on => must not compile as derive");
        assert!(err.contains("Relation reads use"), "{err}");
    }

    #[test]
    fn json_object_dag_root_is_literal_noop_error() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "json-dag-root-noop",
            r#"{"foo":"bar"}"#,
        )
        .expect_err("bare JSON DAG root must be rejected");
        assert!(err.contains("literal no-op"), "{err}");
    }

    #[test]
    fn json_object_root_is_literal_noop_error() {
        let session = test_session();
        let err = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "json-root-noop",
            r#"{"foo":"bar"}"#,
        )
        .expect_err("bare JSON object root must be rejected");
        assert!(err.contains("literal no-op"), "{err}");
    }

    #[test]
    fn scalar_json_bind_still_compiles() {
        let session = test_session();
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "scalar-json-bind",
            r#"cfg = {"k":"v"}
cfg"#,
        )
        .expect("assignment JSON bind should compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let cfg = nodes.iter().find(|n| n["id"] == "cfg").expect("cfg node");
        assert_eq!(cfg["kind"], "data");
    }

    #[test]
    fn get_row_content_reference_errors_with_template_hint() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "get-row-content-hint",
            r#"issue = LangItem("i1")
issue.content"#,
        )
        .expect_err("GET row .content must not look like relation");
        assert!(err.contains("row-to-text template bindings"), "{err}");
    }

    #[test]
    fn derive_map_rejects_session_symbol_entity_ctor() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "derive-reject-e1-ctor",
            r#"hits = LangItem
bad = hits => e1(p5=_.id)
bad"#,
        )
        .expect_err("e1(...) on => must not compile as derive literal");
        assert!(err.contains("derive map does not accept"), "{err}");
    }

    #[test]
    fn surface_relation_chain_with_postfix_compiles() {
        let session = test_session();
        let plan = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "relation-chain-limit",
            r#"LangItem("i1").lines.limit(2)[id]"#,
        )
        .expect("direct relation chain with postfix should compile");
        let nodes = plan.get("nodes").and_then(|v| v.as_array()).expect("nodes");
        assert!(
            nodes.len() >= 2,
            "expected relation + compute nodes, got {nodes:?}"
        );
        assert!(
            !format!("{plan:?}").contains("internal: relation chains must be lowered"),
            "{plan:?}"
        );
    }

    /// Compile `program` and assert its post-`.limit` projection resolved against the relation
    /// **target** entity: compilation succeeds and the plan does not leak the receiver-only field
    /// `team_key`. Shared by the wire/opaque/`query_scoped` cases below.
    fn assert_relation_limit_projection_targets(
        session: &ExecuteSession,
        what: &str,
        program: &str,
    ) {
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            session,
            "relation-hop-limit-project",
            program,
        )
        .unwrap_or_else(|e| {
            panic!("{what}: projection after relation+limit must resolve against the relation target, got: {e}\nprogram:\n{program}")
        });
        assert!(
            !format!("{plan:?}").contains("team_key"),
            "{what}: plan leaked receiver-entity field `team_key`\nprogram:\n{program}"
        );
    }

    /// Regression: a projection after `relation-hop + .limit(...)` must resolve field tokens against
    /// the relation **target**, not the **receiver**. Covers `from_parent_get` and `query_scoped`
    /// materialize with wire names and session `e#` entity roots. Before the fix the limit-compute qe
    /// traced back to the receiver (`LangItem`) and could resolve a homograph to a receiver field
    /// (`team_key`). The projected `note`/`label` fields exist only on the relation targets
    /// (`LangLine`/`LangTag`), never on the `LangItem` receiver.
    #[test]
    fn relation_hop_limit_projection_resolves_against_target_entity() {
        let session = test_session();
        let map = session
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let e_item = map.entity_sym_for("langmatrix", "LangItem");
        let r_lines = map.ident_sym_relation_for("langmatrix", "LangItem", "lines");
        let p_note = map.ident_sym_entity_field_for("langmatrix", "LangLine", "note");
        assert_eq!(p_note, "note", "field teaches as wire name");
        let session_tokens =
            format!("item = {e_item}(\"i1\")\nlines = item.{r_lines}.limit(2)\nlines[{p_note}]");
        for (what, program) in [
            (
                "from_parent_get (wire)",
                "item = LangItem(\"i1\")\nlines = item.lines.limit(2)\nlines[note]",
            ),
            (
                "query_scoped (wire)",
                "item = LangItem(\"i1\")\ntags = item.tags.limit(2)\ntags[label]",
            ),
            (
                "from_parent_get (session e#/r# + wire field)",
                session_tokens.as_str(),
            ),
        ] {
            assert_relation_limit_projection_targets(&session, what, program);
        }
    }

    /// Complement to the positive case: projecting a **receiver** field (`title`, a `LangItem` field)
    /// after the relation hop + limit must be **rejected** against the target row, with a diagnostic
    /// that names the missing row field rather than an unrelated receiver field.
    #[test]
    fn relation_hop_limit_then_separate_projection_resolves_target_entity() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "relation-hop-limit-split-project",
            r#"item = LangItem("i1")
lines = item.lines.limit(2)
bad = lines[title]
bad"#,
        )
        .expect_err("`title` is a LangItem field, not a LangLine row field — must be rejected against the target");
        assert!(
            err.contains("not a row field"),
            "expected target-entity rejection, got: {err}"
        );
        assert!(
            !err.contains("team_key"),
            "diagnostic must not surface unrelated receiver fields: {err}"
        );
    }

    /// Primary session `entry_id` is `github`, but `LangLine` was exposed from `linear` in teaching table
    /// — plan `qualified_entity` must use the owning catalog, not the lexicographic primary.
    #[test]
    fn federated_surface_qualified_entity_matches_exposure_catalog() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("load plasm_language_matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let layers: Vec<&CGS> = vec![cgs.as_ref(), cgs.as_ref()];
        let mut exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["LangItem"]);
        exp.expose_entities(&layers, cgs.clone(), "linear", &["LangLine"]);
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into(), "LangLine".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let map = session
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let e_linear = map.entity_sym_for("linear", "LangLine");
        let plan = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "t",
            &format!(r#"{e_linear}("L1")"#),
        )
        .expect("compile");
        let qe = &plan["nodes"][0]["qualified_entity"];
        assert_eq!(qe["entry_id"], "linear", "{plan}");
        assert_eq!(qe["entity"], "LangLine");
    }

    /// Same wire entity name in two catalogs: session `e1` / `e2` must stamp `qualified_entity` per catalog.
    #[test]
    fn federated_duplicate_entity_name_e_symbol_stamps_catalog_in_plan() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("load plasm_language_matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let layers: Vec<&CGS> = vec![cgs.as_ref(), cgs.as_ref()];
        let mut exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["LangItem"]);
        exp.expose_entities(&layers, cgs.clone(), "linear", &["LangItem"]);
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        for (sym, entry_id) in [("e1", "github"), ("e2", "linear")] {
            let plan = compile_plasm_surface_line_to_plan(
                &PromptPipelineConfig::default(),
                None,
                &session,
                sym,
                sym,
            )
            .unwrap_or_else(|e| panic!("compile {sym}: {e}"));
            let qe = &plan["nodes"][0]["qualified_entity"];
            assert_eq!(qe["entry_id"], entry_id, "plan for {sym}");
            assert_eq!(qe["entity"], "LangItem");
        }
    }

    /// Federated primary is `linear` but relation target `LangDetail` resolves via owning CGS pointer, not primary `entry_id`.
    #[test]
    fn federated_relation_target_qe_from_owning_catalog() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs_primary = Arc::new(
            load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix")).expect("cgs"),
        );
        let cgs_secondary = Arc::new(
            load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix")).expect("cgs"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs_primary.clone())),
        );
        ctxs.insert(
            "pokeapi".into(),
            Arc::new(CgsContext::entry("pokeapi", cgs_secondary.clone())),
        );
        let layers: Vec<&CGS> = vec![cgs_primary.as_ref(), cgs_secondary.as_ref()];
        let mut exp = TeachingExposureSession::new(cgs_primary.as_ref(), "linear", &["LangLine"]);
        exp.expose_entities(&layers, cgs_secondary.clone(), "pokeapi", &["LangItem"]);
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs_primary.clone(),
            ctxs,
            "linear".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into(), "LangLine".into()],
            Some(exp),
            None,
            cgs_primary.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let map = session
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let e_poke = map.entity_sym_for("pokeapi", "LangItem");
        let source = format!(
            r#"item = {e_poke}("LI1")
summary = item.summary
summary"#
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "fed-relation-target",
            &source,
        )
        .expect("compile");
        let summary = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "summary")
            .expect("summary node");
        assert_eq!(summary["kind"], "relation");
        assert_eq!(
            summary["relation"]["target"]["entry_id"], "pokeapi",
            "{summary}"
        );
        assert_eq!(summary["relation"]["target"]["entity"], "LangSummary");
        let ir = summary["relation"]["ir"]["expr"].to_string();
        assert!(
            !ir.contains(r#""$""#),
            "typed continuation IR must not use teaching placeholder: {ir}"
        );
        let plan_value = crate::plasm_plan::parse_plan_value(&plan).expect("parse plan");
        crate::plasm_plan::validate_plan_artifact(&plan_value).expect("validate plan");
        evaluate_plasm_plan_dry(&session, &plan).expect("federated relation dry-run");
    }

    /// Same wire entity in github+linear: relation hop from `e2` binding must target linear catalog.
    #[test]
    fn federated_duplicate_entity_relation_hop_preserves_source_catalog() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("load plasm_language_matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let layers: Vec<&CGS> = vec![cgs.as_ref(), cgs.as_ref()];
        let mut exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["LangItem"]);
        exp.expose_entities(&layers, cgs.clone(), "linear", &["LangItem"]);
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let map = session
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let e2 = map.entity_sym_for("linear", "LangItem");
        let source = format!(
            r#"parent = {e2}("LI1")
kids = parent.children
kids"#
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "fed-dup-relation",
            &source,
        )
        .expect("compile federated duplicate-entity relation hop");
        let kids = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "kids")
            .expect("kids node");
        assert_eq!(kids["kind"], "relation");
        assert_eq!(kids["relation"]["target"]["entry_id"], "linear", "{kids}");
        assert_eq!(kids["relation"]["target"]["entity"], "LangItem");
        evaluate_plasm_plan_dry(&session, &plan).expect("dry-run");
    }

    /// Federated phrase_ident: secondary-catalog bare query + create invoke (issue #23).
    #[test]
    fn federated_secondary_catalog_query_and_create_phrase_ident() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("load plasm_language_matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let layers: Vec<&CGS> = vec![cgs.as_ref(), cgs.as_ref()];
        let mut exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["LangItem"]);
        exp.expose_entities(&layers, cgs.clone(), "linear", &["LangItem"]);
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let map = session
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let e2 = map.entity_sym_for("linear", "LangItem");

        let query_plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "fed-secondary-query",
            e2.as_str(),
        )
        .expect("secondary bare query must compile");
        let query_node = query_plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "return_1")
            .expect("return_1 query node");
        assert_eq!(query_node["kind"], "query");
        assert_eq!(query_node["qualified_entity"]["entry_id"], "linear", "{query_node}");

        let create_m = map.method_sym_for("linear", "LangItem", "langitem_create");
        let unbound = format!(
            r#"bad = {e2}.{create_m}(title=hello)
bad"#
        );
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "fed-secondary-create-unbound",
            &unbound,
        )
        .expect_err("unbound phrase on create param");
        assert!(
            err.contains("unknown program binding") || err.contains("program binding"),
            "{err}"
        );
        assert!(
            !err.contains("unknown capability"),
            "must not false-reject capability on wrong catalog: {err}"
        );

        let bound = format!(
            r#"title = "hello"
created = {e2}.{create_m}(title=title)
created"#
        );
        let create_plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "fed-secondary-create-bound",
            &bound,
        )
        .expect("bound create on secondary catalog");
        let created = create_plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "created")
            .expect("created node");
        assert_eq!(created["kind"], "create");
        assert_eq!(created["qualified_entity"]["entry_id"], "linear", "{created}");
        evaluate_plasm_plan_dry(&session, &create_plan).expect("dry-run");
    }

    /// Real github+linear catalogs: linear `Issue.children` hop from `e2` binding (not github `sub_issues`).
    #[test]
    fn federated_github_linear_issue_children_relation_dry_run() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let github_dir = root.join("../../apis/github");
        let linear_dir = root.join("../../apis/linear");
        if !github_dir.is_dir() || !linear_dir.is_dir() {
            return;
        }
        let cgs_github =
            Arc::new(plasm_core::loader::load_schema_dir(&github_dir).expect("github"));
        let cgs_linear =
            Arc::new(plasm_core::loader::load_schema_dir(&linear_dir).expect("linear"));
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs_github.clone())),
        );
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs_linear.clone())),
        );
        let layers: Vec<&CGS> = vec![cgs_github.as_ref(), cgs_linear.as_ref()];
        let mut exp = TeachingExposureSession::new(cgs_github.as_ref(), "github", &["Issue"]);
        exp.expose_entities(&layers, cgs_linear.clone(), "linear", &["Issue"]);
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs_github.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["Issue".into()],
            Some(exp),
            None,
            cgs_github.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let map = session
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let e2 = map.entity_sym_for("linear", "Issue");
        let r_sym = map.ident_sym_relation_for("linear", "Issue", "children");
        let source = format!(
            r#"parent = {e2}("issue-id")
kids = parent.{r_sym}
kids"#
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "fed-linear-children-real",
            &source,
        )
        .expect("compile real github+linear children hop");
        let kids = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "kids")
            .expect("kids node");
        assert_eq!(kids["relation"]["target"]["entry_id"], "linear", "{kids}");
        evaluate_plasm_plan_dry(&session, &plan).expect("dry-run real catalogs");
    }

    /// Faithful repro of the live Linear failure: real `apis/linear`, multi-entity opaque session
    /// (Issue + Label + Comment), `issue.comments.limit(3)[id,body]` written with opaque `e#`/`r#`/`p#`.
    /// Before the fix this surfaced `team_key` (a receiver Issue field) when validating the Comment
    /// projection after the limit compute.
    #[test]
    fn linear_issue_comments_limit_projection_opaque_resolves_target() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let linear_dir = root.join("../../apis/linear");
        if !linear_dir.is_dir() {
            return;
        }
        let cgs = Arc::new(plasm_core::loader::load_schema_dir(&linear_dir).expect("linear"));
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let exp =
            TeachingExposureSession::new(cgs.as_ref(), "linear", &["Issue", "Label", "Comment"]);
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "linear".into(),
            String::new(),
            String::new(),
            None,
            vec!["Issue".into(), "Label".into(), "Comment".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let map = session
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let e_issue = map.entity_sym_for("linear", "Issue");
        let r_comments = map.ident_sym_relation_for("linear", "Issue", "comments");
        let p_id = map.ident_sym_entity_field_for("linear", "Comment", "id");
        let p_body = map.ident_sym_entity_field_for("linear", "Comment", "body");
        let source = format!(
            r#"issue = {e_issue}("PLASM-1")
comments = issue.{r_comments}.limit(3)
comments[{p_id},{p_body}]"#
        );
        assert_relation_limit_projection_targets(
            &session,
            "linear Issue.comments (real catalog)",
            &source,
        );
    }

    #[test]
    fn lookup_relation_chain_meta_requires_qe_federated() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into()],
            None,
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let chain = plasm_core::ChainExpr::auto_get(
            plasm_core::Expr::Get(plasm_core::GetExpr::new("LangItem", "LI1")),
            "summary".to_string(),
        );
        let err = super::lookup_relation_chain_meta(&session, None, &chain, None).unwrap_err();
        assert!(
            err.contains("federated relation continuation requires catalog ownership"),
            "{err}"
        );
    }

    #[test]
    fn typed_relation_continuation_ir_has_no_domain_placeholder() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits
one = commits.singleton()
author = one.author
author"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "no-dollar-ir",
            source,
        )
        .expect("compile");
        let author = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "author")
            .expect("author");
        let ir = author["relation"]["ir"]["expr"].to_string();
        assert!(
            !ir.contains(r#""$""#),
            "relation IR must not contain Get($): {ir}"
        );
        assert!(ir.contains("author") || ir.contains("Commit"), "{ir}");
    }

    /// Matrix analogue: parallel comma roots + search sugar (`lang_derive_map_parallel`, `lang_search`).
    #[test]
    fn group_by_aggregate_chain_lowers_to_single_group_by_node() {
        let session = test_session();
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "gb-agg-chain",
            "LangItem.group_by(owner).aggregate(n=count)",
        )
        .expect("group_by().aggregate() chain");
        let computes: Vec<_> = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .filter(|n| n["kind"] == "compute")
            .collect();
        assert_eq!(computes.len(), 1, "expected fused GroupBy compute: {plan}");
        assert_eq!(computes[0]["compute"]["op"]["kind"], "group_by", "{plan}");
    }

    #[test]
    fn bare_comma_plasm_roots_compile_as_parallel_return() {
        let session = test_session();
        let plan = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "parallel-roots",
            r#"LangItem, LangItem~"Alpha""#,
        )
        .expect("compile parallel roots");
        assert_eq!(plan["return"]["kind"], "parallel");
    }

    #[test]
    fn rejects_return_prefixed_surface_line() {
        let session = test_session();
        let err = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "t",
            "return LangItem, LangLine",
        )
        .expect_err("return prefix");
        assert!(err.contains("Remove `return`"), "unexpected: {err}");
    }

    #[test]
    fn rejects_return_prefixed_final_roots_in_dag() {
        let session = test_session();
        let source = "items = LangItem\nreturn items";
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "x",
            source,
        )
        .expect_err("return");
        assert!(err.contains("Remove `return`"), "unexpected: {err}");
    }

    fn linear_test_session(cgs: Arc<CGS>) -> ExecuteSession {
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let exp = TeachingExposureSession::new(
            cgs.as_ref(),
            "linear",
            &["Issue", "IssueContext", "MyWorkSnapshot", "Team", "Comment"],
        );
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "linear".into(),
            String::new(),
            String::new(),
            None,
            exp.entities.clone(),
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    /// Linear `Issue{…}` brace filters must plan as `search` (same as live `issue_search` resolution).
    #[test]
    fn linear_issue_brace_filter_plans_as_search() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = root.join("../../apis/linear");
        if !dir.exists() {
            return;
        }
        let cgs = Arc::new(plasm_core::loader::load_schema_dir(&dir).expect("linear"));
        let session = linear_test_session(cgs);
        let plan = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "t",
            "Issue{team_key=ENG, state_name=Todo}",
        )
        .expect("compile");
        assert_eq!(plan["nodes"][0]["kind"].as_str(), Some("search"), "{plan}");
    }

    /// Linear `issue_search` rows include `team_key` in provides so agents can `group_by` on filter dimensions.
    #[test]
    fn linear_issue_search_group_by_team_key_dry_run() {
        fn plan_group_by_keys(plan: &serde_json::Value) -> Vec<String> {
            let mut out = Vec::new();
            let Some(nodes) = plan.get("nodes").and_then(|n| n.as_array()) else {
                return out;
            };
            for node in nodes {
                if node.get("kind").and_then(|k| k.as_str()) != Some("compute") {
                    continue;
                }
                let Some(op) = node.get("compute").and_then(|c| c.get("op")) else {
                    continue;
                };
                if op.get("kind").and_then(|k| k.as_str()) != Some("group_by") {
                    continue;
                }
                let Some(keys) = op.get("keys").and_then(|k| k.as_array()) else {
                    continue;
                };
                for key in keys {
                    if let Some(path) = key.as_array() {
                        if let Some(field) = path.first().and_then(|x| x.as_str()) {
                            out.push(field.to_string());
                        }
                    } else if let Some(s) = key.as_str() {
                        out.push(s.to_string());
                    }
                }
            }
            out
        }

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = root.join("../../apis/linear");
        if !dir.exists() {
            return;
        }
        let cgs = Arc::new(plasm_core::loader::load_schema_dir(&dir).expect("linear"));
        let session = linear_test_session(cgs);
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "linear-search-group-by-team",
            r#"issues = Issue~$
by_team = issues.group_by(team_key)
by_team"#,
        )
        .expect("compile");
        let keys = plan_group_by_keys(&plan);
        assert!(
            keys.iter().any(|k| k == "team_key"),
            "expected group_by on team_key, got {keys:?}; plan={plan}"
        );
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert!(!dry.node_results.is_empty());
    }

    /// Matrix: `lang_domain_symbol_page_size` (surface `e#.page_size` + plan node `page_size`).
    #[test]
    fn surface_line_plan_compiles_e1_with_page_size() {
        let session = test_session();
        let plan = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "t",
            "e1.page_size(100)",
        )
        .expect("compile");
        assert_eq!(plan["nodes"].as_array().map(|a| a.len()), Some(1));
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert!(!dry.node_results.is_empty());
    }

    #[test]
    fn flat_line_projection_on_binding_returns_projection() {
        let session = test_session();
        let source = r#"item = LangItem("i1") lines = item.lines lines[note]"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flat-line-projection",
            source,
        )
        .expect("flat line with trailing projection on in-scope binding");
        let return_node = plan["return"]["node"].as_str().expect("return node id");
        assert_ne!(
            return_node, "item",
            "must return lines projection, not first binding"
        );
        let return_entry = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == return_node)
            .expect("return node in plan");
        assert_eq!(
            return_entry["kind"], "compute",
            "return should be a projection node"
        );
        assert!(
            plan["metadata"].get("coerced_default_return").is_none(),
            "deliberate trailing projection must not record coercion"
        );
    }

    #[test]
    fn flattened_dag_bindings_compile_with_coerced_return() {
        let session = test_session();
        let source = r#"item = LangItem("i1") lines = item.lines lines"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flattened",
            source,
        )
        .expect("flattened space-separated bindings compile on DAG path");
        assert_eq!(plan["return"]["node"], "item");
        assert_eq!(plan["metadata"]["coerced_default_return"], "item");
        let lines = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "lines")
            .expect("lines relation node");
        assert_eq!(lines["relation"]["relation"], "lines");
    }

    #[test]
    fn flattened_dag_assignment_then_root_coerces_first_binding_return() {
        let session = test_session();
        let source = r#"item = LangItem("i1") LangItem.sort(score, desc).limit(2)"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flattened-root",
            source,
        )
        .expect("flattened assignment + postfix root compiles on DAG path");
        assert_eq!(plan["return"]["node"], "item");
        assert_eq!(plan["metadata"]["coerced_default_return"], "item");
    }

    #[test]
    fn flattened_dag_with_multiline_quoted_arg_errors_before_flatten() {
        let session = test_session();
        let source = "prof = LangItem(\"i1\") LangLine(message=\"long\nbody\")";
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flattened-quote",
            source,
        )
        .expect_err("physical newline in quoted arg should fail before flatten");
        assert!(
            err.contains("physical newline inside a quoted Plasm string parameter")
                && err.contains("tagged heredoc"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn multiline_quoted_arg_gets_heredoc_diagnostic() {
        let err = collect_program_statement_lines("body = LangLine(message=\"long\nbody\")")
            .expect_err("physical newline in quote");
        assert!(
            err.contains("physical newline inside a quoted Plasm string parameter")
                && err.contains("tagged heredoc"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn flattened_dag_diagnostic_does_not_mask_heredoc_newline_errors() {
        let session = test_session();
        let source = "body = <<B hello B";
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "heredoc-flat",
            source,
        )
        .expect_err("bad heredoc should fail");
        assert!(
            !err.contains("Do not separate bindings or final roots with spaces"),
            "unexpected: {err}"
        );
    }

    /// Matrix: heredoc / render delimiter hygiene (`lang_bindings_render`, `lang_heredoc_binding`).
    #[test]
    fn split_top_level_does_not_split_commas_inside_tagged_heredoc() {
        let parts = split_top_level("fn(<<T\na,b,c\nT\n), bar", ',').expect("split");
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("a,b,c"), "{:?}", parts[0]);
        assert_eq!(parts[1].trim(), "bar");
    }

    #[test]
    fn collect_program_statement_lines_errors_on_squashed_heredoc_opener() {
        let err = collect_program_statement_lines("body = <<B # junk").expect_err("err");
        assert!(
            err.contains("opener") || err.contains("tag") || err.contains("newline"),
            "{err}"
        );
    }

    #[test]
    fn collect_program_statement_lines_glued_heredoc_close() {
        // `H)` closes the heredoc and ends with `)`; outer `m(` balances that delimiter.
        let stmts = collect_program_statement_lines("x = m(<<H\none\nH)").expect("parse");
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("<<H"), "{:?}", stmts[0]);
        assert!(stmts[0].contains("one"));
    }

    /// Matrix: heredoc binding + parallel roots (`lang_heredoc_binding`).
    #[test]
    fn multiline_heredoc_binding_then_parallel_roots_compiles() {
        let session = test_session();
        let source = "body = <<T\nhello\nT\nLangItem, LangLine(\"L1\")";
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "heredoc-roots",
            source,
        )
        .expect("compile");
        assert_eq!(plan["return"]["kind"], "parallel");
    }

    fn github_repository_commit_session() -> ExecuteSession {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(load_schema(&root.join("../../apis/github")).expect("load github"));
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        let exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["Repository", "Commit"]);
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["Repository".into(), "Commit".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    #[test]
    fn compiles_two_hop_one_cardinality_relation_binding_chain() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits
one = commits.singleton()
author = one.author
author"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-two-hop-one-rel",
            source,
        )
        .expect("compile");
        let author = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "author")
            .expect("author relation node");
        assert_eq!(author["kind"], "relation");
        assert_eq!(
            author["relation"]["source_cardinality"].as_str(),
            Some("runtime_checked_singleton")
        );
        assert_eq!(author["relation"]["cardinality"], "one");
        let ir = author["relation"]["ir"]["expr"].to_string();
        assert!(!ir.contains(r#""$""#), "author IR: {ir}");
        let plan_value = crate::plasm_plan::parse_plan_value(&plan).expect("parse plan");
        crate::plasm_plan::validate_plan_artifact(&plan_value).expect("validate plan");
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(
            dry.node_results.len(),
            plan["nodes"].as_array().unwrap().len()
        );
    }

    /// `repo.<relation>` continues the bound repository Plasm and compiles to a `kind: relation` plan node.
    #[test]
    fn compiles_bound_node_ref_relation_chain_dag_to_valid_plan() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits
commits"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-node-ref-rel",
            source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        assert_eq!(nodes.len(), 2, "{plan:#}");
        let rel = &nodes[1];
        assert_eq!(rel["kind"], "relation");
        assert_eq!(rel["relation"]["source"], "repo");
        assert_eq!(rel["relation"]["relation"], "commits");
        assert_eq!(rel["relation"]["target"]["entity"], "Commit");
        assert_eq!(rel["uses_result"][0]["node"], "repo");
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(
            dry.node_results[1]["simulation"]["kind"],
            "relation_traversal"
        );
    }

    #[test]
    fn binding_method_invoke_is_referentially_transparent() {
        let session = test_session();
        let explicit = r#"LangItem("i1").update(title="MatrixPatch", score=42, owner="alice")"#;
        let bound = r#"item = LangItem("i1")
out = item.update(title="MatrixPatch", score=42, owner="alice")
out"#;
        let plan_explicit = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "rt-explicit",
            explicit,
        )
        .expect("explicit compile");
        let plan_bound = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "rt-bound",
            bound,
        )
        .expect("bound compile");
        let ir_explicit = &plan_explicit["nodes"].as_array().unwrap()[0]["ir"]["expr"];
        let ir_bound = &plan_bound["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "out")
            .expect("out node")["ir"]["expr"];
        assert_eq!(
            ir_explicit, ir_bound,
            "bound method invoke must lower to same IR as explicit anchor"
        );
    }

    #[test]
    fn binding_method_invoke_with_binding_field_ref_in_args() {
        let session = test_session();
        let explicit = r#"item = LangItem("i1")
peer = LangItem("i2")
LangItem("i1").update(title=peer.title, score=42, owner="alice")"#;
        let bound = r#"item = LangItem("i1")
peer = LangItem("i2")
out = item.update(title=peer.title, score=42, owner="alice")
out"#;
        let plan_explicit = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "rt-explicit-field-ref",
            explicit,
        )
        .expect("explicit compile");
        let plan_bound = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "rt-bound-field-ref",
            bound,
        )
        .expect("bound compile");
        let ir_explicit = plan_explicit["nodes"]
            .as_array()
            .unwrap()
            .last()
            .expect("surface node")["ir"]["expr"]
            .clone();
        let ir_bound = plan_bound["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "out")
            .expect("out node")["ir"]["expr"]
            .clone();
        assert_eq!(
            ir_explicit, ir_bound,
            "bound method invoke with field-ref args must lower to same IR as explicit anchor"
        );
    }

    #[test]
    fn binding_plural_side_effect_method_invoke_rejected() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "rt-plural-side-effect",
            r#"items = LangItem
bad = items.update(title="x", score=1, owner="a")
bad"#,
        )
        .expect_err("plural binding must not fan out side effects");
        assert!(
            err.contains("singleton"),
            "expected singleton side-effect gate, got: {err}"
        );
    }

    #[test]
    fn binding_continuation_unknown_tail_emits_plp4() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "rt-plp4-unknown",
            r#"items = LangItem
one = items.limit(1)
bad = one.foo.bar
bad"#,
        )
        .expect_err("multi-segment continuation on binding-label anchor must fail");
        assert!(
            err.contains("PLP-4:"),
            "expected PLP-4 diagnostic prefix, got: {err}"
        );
    }

    #[test]
    fn github_commit_query_path_filter_symbols_distinct_and_map_to_path_wire() {
        use plasm_core::expr_parser::parse_with_cgs_layers;

        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let path_p = map.ident_sym_cap_param_for("github", "Commit", "commit_query", "path");
        let author_p = map.ident_sym_cap_param_for("github", "Commit", "commit_query", "by_author");
        let committer_p =
            map.ident_sym_cap_param_for("github", "Commit", "commit_query", "by_committer");
        assert_ne!(
            path_p, committer_p,
            "path and by_committer must not share p#"
        );
        assert_ne!(path_p, author_p, "path and by_author must not share p#");

        let commit_e = map.entity_sym_for("github", "Commit");
        let repo_e = map.entity_sym_for("github", "Repository");
        let owner_f = map.ident_sym_entity_field_for("github", "Repository", "owner");
        let repo_f = map.ident_sym_entity_field_for("github", "Repository", "repo");
        let repo_param =
            map.ident_sym_cap_param_for("github", "Commit", "commit_query", "repository");
        let expr = format!(
            r#"{commit_e}{{{repo_param}={repo_e}({owner_f}=octocat, {repo_f}=Hello-World), {path_p}="README.md"}}"#
        );
        let stack = crate::plasm_plan_run::session_cgs_layer_stack(&session);
        let parsed =
            parse_with_cgs_layers(&expr, &stack, map).expect("parse commit_query path filter");
        let plasm_core::Expr::Query(q) = &parsed.expr else {
            panic!("expected query, got {:?}", parsed.expr);
        };
        assert_eq!(q.entity, "Commit");
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let path_field_ok = match pred {
            plasm_core::Predicate::Comparison { field, .. } => field == "path",
            plasm_core::Predicate::And { args } => args.iter().any(|p| {
                matches!(
                    p,
                    plasm_core::Predicate::Comparison { field, .. } if field == "path"
                )
            }),
            _ => false,
        };
        assert!(
            path_field_ok,
            "path filter must bind path param, got: {pred:?}"
        );

        let program = format!(
            r#"rows = {commit_e}{{{repo_param}={repo_e}({owner_f}=octocat, {repo_f}=Hello-World), {path_p}="README.md"}}
rows"#
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "commit-path-filter-dry",
            &program,
        )
        .expect("compile commit path filter");
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        let dry_str = serde_json::to_string(&dry.node_results).expect("dry json");
        assert!(
            dry_str.contains("path") || dry_str.contains(&path_p),
            "dry-run should surface path filter wire, got: {dry_str}"
        );
        assert!(
            !dry_str.contains("by_committer"),
            "dry-run must not show committer param for path filter: {dry_str}"
        );
    }

    #[test]
    fn relation_uses_result_includes_scope_binding_aliases() {
        let session = github_issue_label_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
issues = Issue{repository=repo.full_name}
labels = issues.labels
labels"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-issue-label-scope",
            source,
        )
        .expect("compile");
        let labels = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "labels")
            .expect("labels relation node");
        let uses = labels["uses_result"].as_array().expect("uses_result");
        assert!(
            uses.iter()
                .any(|u| u["node"] == "repo" && u["as"] == "repo"),
            "expected repo in uses_result: {uses:?}"
        );
        assert!(
            uses.iter()
                .any(|u| u["node"] == "issues" && u["as"] == "source"),
            "expected issues source in uses_result: {uses:?}"
        );
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        let facts = dry
            .graph_summary
            .get("boundedness_facts")
            .and_then(|v| v.as_array())
            .expect("boundedness_facts");
        let joined: Vec<String> = facts
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        assert!(
            joined
                .iter()
                .any(|f: &String| f.contains("Includes relation traversal")),
            "expected relation-traversal boundedness fact: {joined:?}"
        );
    }

    #[test]
    fn lhs_gated_relation_segment_ignores_wrong_token() {
        let session = github_issue_label_session();
        let qe = QualifiedEntityKey {
            entry_id: "github".into(),
            entity: "Issue".into(),
        };
        let wire = resolve_relation_segment_for_continuation(
            &session,
            None,
            &qe,
            "p99",
            Some(plasm_core::ProgramBindingLabel("labels")),
        )
        .expect("binding label selects relation wire");
        assert_eq!(wire, "labels");
    }

    /// Shared filter/relation wire resolves as relation in nav; legacy `p#` rejected; DAG LHS binding forgives wrong token.
    #[test]
    fn homograph_p_rejected_in_parse_forgiven_with_lhs_binding_label() {
        use plasm_core::expr_parser::parse_with_cgs_layers_program;
        use plasm_core::relation_segment::{
            resolve_relation_segment, RelationSegmentContext, RelationSegmentOutcome,
        };

        let session = github_issue_label_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let labels_wire = map.ident_sym_cap_param_for("github", "Issue", "issue_query", "labels");
        assert_eq!(
            labels_wire, "labels",
            "labels filter param teaches as catalog wire name"
        );
        let issue_e = map.entity_sym_for("github", "Issue");
        let stack = crate::plasm_plan_run::session_cgs_layer_stack(&session);
        let line = format!("{issue_e}.{labels_wire}");
        parse_with_cgs_layers_program(&line, &stack, map.clone(), None, false)
            .expect("shared labels wire resolves as relation in nav");

        let issue_ent = session.cgs.get_entity("Issue").expect("Issue");
        let ctx = RelationSegmentContext {
            map: &map,
            entity: "Issue",
            relations: &issue_ent.relations,
            binding_label: None,
            allow_lhs_coercion: false,
        };
        assert!(matches!(
            resolve_relation_segment(&ctx, labels_wire.as_str()),
            RelationSegmentOutcome::Wire(w) if w == "labels"
        ));

        let legacy = format!("{issue_e}.p1");
        parse_with_cgs_layers_program(&legacy, &stack, map.clone(), None, false)
            .expect_err("legacy opaque p# must not resolve in relation nav");

        let qe = QualifiedEntityKey {
            entry_id: "github".into(),
            entity: "Issue".into(),
        };
        let wire = resolve_relation_segment_for_continuation(
            &session,
            None,
            &qe,
            "p99",
            Some(plasm_core::ProgramBindingLabel("labels")),
        )
        .expect("LHS binding label selects relation wire");
        assert_eq!(wire, "labels");
    }

    #[test]
    fn multiline_explicit_return_position_not_first_binding() {
        let session = test_session();
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "ml-return-position-limit-projection",
            r#"item = LangItem("i1")
lines = item.lines.limit(2)
lines"#,
        )
        .expect("compile multiline program with explicit trailing return");
        assert_eq!(
            plan["return"]["node"], "lines",
            "final line is return position; must not coerce to first binding `item`"
        );
        assert!(
            plan["metadata"].get("coerced_default_return").is_none(),
            "explicit roots line must not record coercion"
        );
    }

    /// Multi-root return with opaque `[p#,…]` must compile regardless of comma-root order.
    #[test]
    fn multi_return_opaque_projection_order_independent() {
        let session = test_session();
        let exp = session.teaching_exposure.as_ref().expect("exposure");
        let map = exp.symbol_map_arc();
        let e_item = map.entity_sym_for("langmatrix", "LangItem");
        let p_id = map.ident_sym_entity_field_for("langmatrix", "LangItem", "id");
        let p_line_id = map.ident_sym_entity_field_for("langmatrix", "LangLine", "id");
        let p_note = map.ident_sym_entity_field_for("langmatrix", "LangLine", "note");
        let bindings = format!(
            "item = {e_item}({p_id}=\"i1\")\nlines = item.lines\n",
            e_item = e_item,
            p_id = p_id,
        );
        let program_a = format!(
            "{bindings}item, lines[{p_line_id},{p_note}]",
            bindings = bindings,
            p_line_id = p_line_id,
            p_note = p_note,
        );
        let program_b = format!(
            "{bindings}lines[{p_line_id},{p_note}], item",
            bindings = bindings,
            p_line_id = p_line_id,
            p_note = p_note,
        );
        compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "multi-return-a",
            &program_a,
        )
        .unwrap_or_else(|e| panic!("lang-before-projected-relation must compile: {e}"));
        compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "multi-return-b",
            &program_b,
        )
        .expect("projected-relation-before-lang must compile");
    }

    /// Relation fanout row projection accepts entity witness `p#` taught on the target entity.
    #[test]
    fn relation_fanout_projection_accepts_entity_witness_p_symbols() {
        let session = test_session();
        let exp = session.teaching_exposure.as_ref().expect("exposure");
        let map = exp.symbol_map_arc();
        let e_item = map.entity_sym_for("langmatrix", "LangItem");
        let p_id = map.ident_sym_entity_field_for("langmatrix", "LangItem", "id");
        let p_note = map.ident_sym_entity_field_for("langmatrix", "LangLine", "note");
        let source = format!(
            "item = {e_item}({p_id}=\"i1\")\nlines = item.lines\nlines[{p_note}]",
            e_item = e_item,
            p_id = p_id,
            p_note = p_note,
        );
        let err_msg = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "relation-fanout-p",
            &source,
        )
        .map(|_| String::new())
        .unwrap_or_else(|e| e);
        assert!(
            !err_msg.contains("not a row symbol"),
            "entity witness p# must not surface as unknown row symbol: {err_msg}"
        );
    }

    #[test]
    fn flattened_single_liner_lhs_gated_relation_primary_return() {
        let session = github_issue_label_session();
        let source = r#"repo = Repository(owner="octocat", repo="Hello-World") issues = Issue{repository=repo.full_name} labels = issues.labels labels"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-flattened-labels",
            source,
        )
        .expect("flattened single-liner");
        assert_eq!(plan["return"]["node"], "repo");
        assert_eq!(plan["metadata"]["coerced_default_return"], "repo");
        let labels = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "labels")
            .expect("labels node");
        assert_eq!(labels["relation"]["relation"], "labels");
    }

    #[test]
    fn surface_line_compile_matches_dag_for_flattened_single_liner() {
        let session = test_session();
        let source = "items = LangItem tags = items.tags tags";
        let dag = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flatten-parity-dag",
            source,
        )
        .expect("dag compile");
        let surface = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flatten-parity-surface",
            source,
        )
        .expect("surface line compile");
        assert_eq!(surface["return"], dag["return"]);
        assert_eq!(
            surface["metadata"]["coerced_default_return"],
            dag["metadata"]["coerced_default_return"]
        );
        assert_eq!(
            surface["nodes"].as_array().map(|n| n.len()),
            dag["nodes"].as_array().map(|n| n.len())
        );
    }

    #[test]
    fn relation_plural_opaque_p2_continuation() {
        let session = github_issue_label_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let sym = map.ident_sym_relation_for("langmatrix", "Issue", "labels");
        let source = format!(
            r#"repo = Repository(owner="octocat", repo="Hello-World")
issues = Issue{{repository=repo.full_name}}
labels = issues.{sym}
labels"#
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-issue-labels-opaque-p",
            &source,
        )
        .expect("compile opaque plural relation continuation");
        let labels = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "labels")
            .expect("labels relation node");
        assert_eq!(labels["relation"]["relation"], "labels");
        assert_eq!(
            labels["relation"]["source_cardinality"].as_str(),
            Some("many")
        );
        assert_eq!(labels["relation"]["source"], "issues");
        evaluate_plasm_plan_dry(&session, &plan).expect("dry");
    }

    fn language_matrix_tags_session() -> ExecuteSession {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("load plasm_language_matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "langmatrix".into(),
            Arc::new(CgsContext::entry("langmatrix", cgs.clone())),
        );
        let exp =
            TeachingExposureSession::new(cgs.as_ref(), "langmatrix", &["LangItem", "LangTag"]);
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "langmatrix".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into(), "LangTag".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    #[test]
    fn language_matrix_plural_opaque_relation_continuation() {
        let session = language_matrix_tags_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let sym = map.ident_sym_relation_for("langmatrix", "LangItem", "tags");
        let source = format!("items = LangItem\ntags = items.{sym}\ntags");
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "matrix-plural-opaque-tags",
            &source,
        )
        .expect("compile matrix opaque plural relation continuation");
        let tags = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "tags")
            .expect("tags relation node");
        assert_eq!(tags["relation"]["relation"], "tags");
        assert_eq!(
            tags["relation"]["source_cardinality"].as_str(),
            Some("many")
        );
    }

    #[test]
    fn compiles_node_ref_relation_limit_and_project() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits
limited = commits.limit(20)
projected = limited[sha,message]
projected"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-chain-limit-project",
            source,
        )
        .expect("compile");
        assert_eq!(plan["nodes"].as_array().map(Vec::len), Some(4));
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(dry.node_results.len(), 4, "{dry:?}");
    }

    #[test]
    fn bare_label_singleton_lowers_to_limit_preserving_commit_entity() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits.limit(3)
one = commits.singleton()
one"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "bare-label-singleton",
            source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let one = nodes.iter().find(|n| n["id"] == "one").expect("one");
        assert_eq!(one["kind"], "compute");
        assert_eq!(one["compute"]["source"], "commits");
        assert_eq!(one["compute"]["op"]["kind"], "limit");
        assert_eq!(one["compute"]["op"]["count"], 1);
        assert_eq!(one["compute"]["schema"]["entity"], json!("Commit"));
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(dry.node_results.len(), nodes.len());
    }

    /// Matrix: `lang_bind_limit1_continuation` — limit on a surface bind must keep LangItem entity for `.tags` continuation.
    #[test]
    fn limit_on_surface_bind_preserves_langitem_entity() {
        let session = test_session();
        let source = r#"root = LangItem{owner="alice"}
one = root.limit(1)
tags = one.tags
tags"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "limit-surface-bind",
            source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let one = nodes.iter().find(|n| n["id"] == "one").expect("one");
        assert_eq!(one["kind"], "compute");
        assert_eq!(one["compute"]["op"]["kind"], "limit");
        assert_eq!(one["compute"]["schema"]["entity"], json!("LangItem"));
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(dry.node_results.len(), nodes.len());
    }

    #[test]
    fn bare_label_page_size_lowers_to_identity_project() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits.limit(5)
paged = commits.page_size(10)
paged"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "bare-label-page-size",
            source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let paged = nodes.iter().find(|n| n["id"] == "paged").expect("paged");
        assert_eq!(paged["kind"], "compute");
        assert_eq!(paged["compute"]["op"]["kind"], "project");
        assert_eq!(paged["compute"]["page_size"], json!(10));
        assert_eq!(paged["compute"]["schema"]["entity"], json!("Commit"));
    }

    #[test]
    fn bracket_render_accepts_bare_label_singleton_on_source() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field_for("langmatrix", "Commit", "sha");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(2)\nmail = commits.singleton()[{p_sha}] <<MD\nx\nMD\nmail"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "bare-label-singleton-render",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let prefix = nodes
            .iter()
            .find(|n| n["id"].as_str() == Some("__plasm_render_src_mail"))
            .expect("render prefix");
        assert_eq!(prefix["compute"]["op"]["kind"], "limit");
        let render = nodes.iter().find(|n| n["id"] == "mail").expect("mail");
        assert_eq!(render["compute"]["op"]["kind"], "render");
        assert!(
            render["compute"]["page_size"].is_null(),
            "expected render compute.page_size omitted when prefix lowered tail flags"
        );
    }

    #[test]
    fn bracket_render_content_rejected_as_program_root_with_actionable_copy() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field_for("langmatrix", "Commit", "sha");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(1)\nmail = commits[{p_sha}] <<MD\nx\nMD\nmail.content"
        );
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "render-content-root",
            &source,
        )
        .expect_err("mail.content must not be a root");
        assert!(
            err.contains("Don't return `mail.content`"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn derive_accepts_render_content_as_binding_rhs() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field_for("langmatrix", "Commit", "sha");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(1)\nmail = commits[{p_sha}] <<MD\nx\nMD\nout = mail => mail.content\nout"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "derive-render-content",
            &source,
        )
        .expect("derive with mail.content");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let derive = nodes.iter().find(|n| n["id"] == "out").expect("derive");
        assert_eq!(derive["kind"], "derive");
    }

    /// teaching table `p#` inside postfix projection must lower to wire field names in `Plan` IR (not
    /// survive as literal `p#` paths that dry-run would project as null).
    #[test]
    fn dag_postfix_projection_expands_domain_field_symbols_to_wire_paths() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field_for("langmatrix", "Commit", "sha");
        let p_msg = map.ident_sym_entity_field_for("langmatrix", "Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(2)\ncommits[{p_sha},{p_msg}]"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-domain-projection",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let last = nodes.last().expect("compute projection node");
        assert_eq!(last["kind"], "compute");
        let op = &last["compute"]["op"];
        assert_eq!(op["kind"], "project");
        let fields = op["fields"].as_object().expect("project fields");
        assert!(
            fields.contains_key("sha") && fields.contains_key("message"),
            "expected wire keys sha/message, got {fields:?}"
        );
        if p_sha != "sha" {
            assert!(
                !fields.contains_key(&p_sha),
                "teaching-table symbol {p_sha} must not appear as projection column: {fields:?}"
            );
        }
    }

    #[test]
    fn dag_postfix_sort_expands_domain_field_symbol_in_sort_key() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_msg = map.ident_sym_entity_field_for("langmatrix", "Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(3)\nordered = commits.sort({p_msg}, desc)\nordered"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-domain-sort",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let sort_node = nodes
            .iter()
            .find(|n| n["id"] == "ordered")
            .expect("sort node");
        let op = &sort_node["compute"]["op"];
        assert_eq!(op["kind"], "sort");
        assert_eq!(op["key"], json!(["message"]));
        assert_eq!(op["descending"], true);
    }

    #[test]
    fn dag_postfix_sort_whitespace_direction_expands_domain_field_symbol() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_msg = map.ident_sym_entity_field_for("langmatrix", "Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(3)\nordered = commits.sort({p_msg} desc)\nordered"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-domain-sort-whitespace",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let sort_node = nodes
            .iter()
            .find(|n| n["id"] == "ordered")
            .expect("sort node");
        let op = &sort_node["compute"]["op"];
        assert_eq!(op["kind"], "sort");
        assert_eq!(op["key"], json!(["message"]));
        assert_eq!(op["descending"], true);
    }

    #[test]
    fn dag_postfix_sort_on_projected_binding_accepts_p_symbol() {
        let session = test_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_id = map.ident_sym_entity_field_for("langmatrix", "LangItem", "id");
        let p_score = map.ident_sym_entity_field_for("langmatrix", "LangItem", "score");
        let source = format!(
            "rows = LangItem.limit(5)\nnarrow = rows[{p_id},{p_score}]\nordered = narrow.sort({p_score} desc)\nordered"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "matrix-projected-sort",
            &source,
        )
        .expect("compile");
        let sort_node = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "ordered")
            .expect("sort node");
        let op = &sort_node["compute"]["op"];
        assert_eq!(op["kind"], "sort");
        assert_eq!(op["key"], json!(["score"]));
        assert_eq!(op["descending"], true);
    }

    #[test]
    fn dag_postfix_group_by_filter_dedupe_accept_p_symbols() {
        let session = test_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_owner = map.ident_sym_entity_field_for("langmatrix", "LangItem", "owner");
        let p_score = map.ident_sym_entity_field_for("langmatrix", "LangItem", "score");
        let p_id = map.ident_sym_entity_field_for("langmatrix", "LangItem", "id");
        for (name, source) in [
            (
                "group_by",
                format!("rows = LangItem.limit(5)\nout = rows.group_by({p_owner})\nout"),
            ),
            (
                "filter",
                format!("rows = LangItem.limit(5)\nout = rows.filter{{{p_score}>0}}\nout"),
            ),
            (
                "dedupe",
                format!("rows = LangItem.limit(5)\nout = rows.dedupe({p_id})\nout"),
            ),
        ] {
            compile_plasm_dag_to_plan(
                &PromptPipelineConfig::default(),
                None,
                &session,
                name,
                &source,
            )
            .unwrap_or_else(|e| panic!("{name} should accept p# symbols: {e}"));
        }
    }

    #[test]
    fn sort_field_error_recommends_p_symbols_not_projected_columns() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "sort-bad-field",
            "rows = LangItem.limit(2)\nsorted = rows.sort(not_a_field desc)\nsorted",
        )
        .expect_err("unknown sort field");
        assert!(
            err.contains("wire")
                || err.contains("teaching")
                || err.contains("rows:")
                || err.contains("Intermediate step must be a binding"),
            "expected sort guidance, got: {err}"
        );
    }

    #[test]
    fn dag_postfix_aggregate_expands_domain_field_symbol_in_sum() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_add = map.ident_sym_entity_field_for("langmatrix", "Commit", "stats_additions");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(5)\ntot = commits.aggregate(t=sum({p_add}))\ntot"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-domain-aggregate",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let agg = nodes
            .iter()
            .find(|n| n["id"] == "tot")
            .expect("aggregate node");
        let op = &agg["compute"]["op"];
        assert_eq!(op["kind"], "aggregate");
        let aggs = op["aggregates"].as_array().expect("aggregates");
        assert_eq!(aggs[0]["name"], "t");
        assert_eq!(aggs[0]["function"], "sum");
        assert_eq!(aggs[0]["field"], json!(["stats_additions"]));
    }

    #[test]
    fn dag_render_field_list_expands_domain_field_symbols() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field_for("langmatrix", "Commit", "sha");
        let p_msg = map.ident_sym_entity_field_for("langmatrix", "Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(1)\nout = commits[{p_sha},{p_msg}] <<MD\n{{{{ rows | length }}}}\nMD\nout"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-domain-render",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let render = nodes
            .iter()
            .find(|n| n["id"] == "out")
            .expect("render node");
        let op = &render["compute"]["op"];
        assert_eq!(op["kind"], "render");
        let cols = op["columns"].as_array().expect("columns");
        let col_names: Vec<_> = cols
            .iter()
            .map(|v| v.as_str().unwrap_or_default())
            .collect();
        assert_eq!(col_names, vec!["sha", "message"]);
    }

    #[test]
    fn dag_render_infers_columns_from_projected_binding() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field_for("langmatrix", "Commit", "sha");
        let p_msg = map.ident_sym_entity_field_for("langmatrix", "Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(2)[{p_sha},{p_msg}]\nreport = commits <<MD\n{{{{ rows | length }}}}\nMD\nreport"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-inferred-render-projection",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let render = nodes
            .iter()
            .find(|n| n["id"] == "report")
            .expect("render node");
        let op = &render["compute"]["op"];
        assert_eq!(op["kind"], "render");
        let cols = op["columns"].as_array().expect("columns");
        let mut col_names: Vec<_> = cols
            .iter()
            .map(|v| v.as_str().unwrap_or_default())
            .collect();
        col_names.sort();
        assert_eq!(col_names, vec!["message", "sha"]);
    }

    #[test]
    fn dag_render_infers_entity_row_columns_after_limit_only() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits.limit(20)
report = commits <<MD
{{ rows | length }}
MD
report"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-inferred-render-limit",
            source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let render = nodes
            .iter()
            .find(|n| n["id"] == "report")
            .expect("render node");
        let op = &render["compute"]["op"];
        assert_eq!(op["kind"], "render");
        let cols = op["columns"].as_array().expect("columns");
        assert!(
            cols.len() >= 2,
            "expected entity-backed columns (got {cols:?})"
        );
        let names: Vec<_> = cols.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"sha"), "{names:?}");
        assert!(names.contains(&"message"), "{names:?}");
    }

    #[test]
    fn dag_render_node_ref_postfix_explicit_columns_before_heredoc() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field_for("langmatrix", "Commit", "sha");
        let p_msg = map.ident_sym_entity_field_for("langmatrix", "Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits\nreport = commits.limit(20)[{p_sha},{p_msg}] <<MD\nx\nMD\nreport"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-render-chain-binding",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let render = nodes
            .iter()
            .find(|n| n["id"] == "report")
            .expect("render node");
        let op = &render["compute"]["op"];
        assert_eq!(op["kind"], "render");
        let cols = op["columns"].as_array().expect("columns");
        let col_names: Vec<_> = cols
            .iter()
            .map(|v| v.as_str().unwrap_or_default())
            .collect();
        assert_eq!(col_names, vec!["sha", "message"]);
    }

    #[test]
    fn dag_render_rejects_inference_from_prior_render_output() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field_for("langmatrix", "Commit", "sha");
        let p_msg = map.ident_sym_entity_field_for("langmatrix", "Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(1)\nfirst = commits[{p_sha},{p_msg}] <<MD\n{{{{ r.sha }}}}\nMD\nbad = first <<MD\ny\nMD\nbad"
        );
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-render-on-render",
            &source,
        )
        .expect_err("render from render");
        assert!(
            err.contains("cannot infer template columns")
                || err.contains("row-to-text template result"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn compiles_continuation_from_projection_anchor() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
trimmed = repo[id]
commits = trimmed.commits
commits"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "projection-anchor",
            source,
        )
        .expect("projection anchor should compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let rel = nodes
            .iter()
            .find(|n| n["id"] == "commits")
            .expect("relation node");
        assert_eq!(rel["kind"], "relation");
        assert_eq!(rel["relation"]["source"], "trimmed");
        assert_eq!(rel["relation"]["relation"], "commits");
        assert_eq!(rel["relation"]["source_cardinality"], "single");
        assert_eq!(rel["uses_result"][0]["node"], "trimmed");
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        let text = render_plasm_plan_dry_text(&dry, None);
        assert!(text.contains("trimmed.commits"), "{text}");
    }

    #[test]
    fn rejects_continuation_from_aggregate_anchor() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits
totals = commits.aggregate(n=count)
bad = totals.commits"#;
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "aggregate-non-anchor",
            source,
        )
        .expect_err("aggregate is not a Plasm anchor");
        assert!(
            err.contains("row-preserving projection bindings"),
            "unexpected: {err}"
        );
    }

    /// Direct postfix `.limit` on a surface expression must compile with the same plan shape as
    /// bind-first `label = expr` then `label.limit(n)` (unified language contract).
    #[test]
    fn direct_surface_limit_equivalent_to_bind_first_two_node_plan() {
        let session = github_repository_commit_session();
        let bind_first = r#"commits = Repository(owner="ryan-s-roberts", repo="plasm-core").commits
x = commits.limit(2)
x"#;
        let direct = r#"Repository(owner="ryan-s-roberts", repo="plasm-core").commits.limit(2)"#;
        let p1 = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "bind-first-limit",
            bind_first,
        )
        .expect("bind-first");
        let p2 = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "direct-limit",
            direct,
        )
        .expect("direct");
        let n1 = p1["nodes"].as_array().expect("nodes");
        let n2 = p2["nodes"].as_array().expect("nodes");
        assert_eq!(n1.len(), 3, "{p1:#}");
        assert_eq!(n2.len(), 3, "{p2:#}");
        assert_eq!(n1[1]["kind"], "relation");
        assert_eq!(n2[1]["kind"], "relation");
        let last1 = &n1[2];
        let last2 = &n2[2];
        assert_eq!(last1["kind"], "compute");
        assert_eq!(last2["kind"], "compute");
        let op1 = &last1["compute"]["op"];
        let op2 = &last2["compute"]["op"];
        assert_eq!(op1["kind"], "limit", "{op1:#}");
        assert_eq!(op2["kind"], "limit", "{op2:#}");
        assert_eq!(op1["count"], 2);
        assert_eq!(op2["count"], 2);
    }

    #[test]
    fn parse_aggregates_canonical_n_count() {
        let specs = super::parse_aggregates("n=count").expect("canonical count");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name.as_str(), "n");
        assert_eq!(
            specs[0].function,
            crate::plasm_plan::AggregateFunction::Count
        );
        assert!(specs[0].field.is_none());
    }

    #[test]
    fn parse_aggregates_shadow_bare_count() {
        let specs = super::parse_aggregates("count").expect("shadow bare count");
        assert_eq!(specs[0].name.as_str(), "count");
        assert_eq!(
            specs[0].function,
            crate::plasm_plan::AggregateFunction::Count
        );
    }

    #[test]
    fn parse_aggregates_shadow_aggregate_count() {
        let specs = super::parse_aggregates("aggregate(count)").expect("shadow aggregate(count)");
        assert_eq!(specs[0].name.as_str(), "count");
    }

    #[test]
    fn parse_aggregates_rejects_aggregate_sum_without_alias() {
        let err = super::parse_aggregates("aggregate(sum(amount))").unwrap_err();
        assert!(
            err.contains("total=sum(amount)") || err.contains("explicit"),
            "{err}"
        );
    }

    #[test]
    fn compile_row_filter_brace_on_binding() {
        let session = test_session();
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "lang_row_filter_brace",
            "items = LangItem\nfiltered = items.filter{owner=\"o1\"}\nfiltered",
        )
        .expect("compile filter program");
        let has_filter = plan["nodes"].as_array().expect("nodes").iter().any(|n| {
            n.get("compute")
                .and_then(|c| c.get("op"))
                .and_then(|o| o.get("kind"))
                == Some(&serde_json::json!("filter"))
        });
        assert!(has_filter, "expected filter compute node: {plan}");
    }

    #[test]
    fn group_by_postfix_accepts_canonical_key_and_aggs() {
        let session = test_session();
        let pipeline = PromptPipelineConfig::default();
        let state = super::CompileState::new(&pipeline, None);
        let node = super::postfix_op_to_compute(
            &session,
            &state,
            &[],
            &plasm_core::expr_parser::PlasmPostfixOp::GroupBy {
                args: "owner, n=count".into(),
            },
            "src",
            "id",
            "expr",
        )
        .expect("group_by");
        match node.source {
            super::DagNodeSource::Compute {
                op:
                    crate::plasm_plan::ComputeOp::GroupBy {
                        ref keys,
                        ref aggregates,
                    },
                ..
            } => {
                assert_eq!(keys.len(), 1);
                assert_eq!(keys[0].dotted(), "owner");
                assert_eq!(aggregates.len(), 1);
                assert_eq!(aggregates[0].name.as_str(), "n");
            }
            _ => panic!("expected group_by compute"),
        }
    }

    #[test]
    fn sort_unknown_direction_errors() {
        let session = test_session();
        let pipeline = PromptPipelineConfig::default();
        let state = super::CompileState::new(&pipeline, None);
        let err = super::postfix_op_to_compute(
            &session,
            &state,
            &[],
            &plasm_core::expr_parser::PlasmPostfixOp::Sort {
                args: "score, newest".into(),
            },
            "src",
            "id",
            "expr",
        )
        .unwrap_err();
        assert!(err.contains("newest"), "{err}");
    }

    #[test]
    fn sort_accepts_direction_aliases() {
        let session = test_session();
        let pipeline = PromptPipelineConfig::default();
        let state = super::CompileState::new(&pipeline, None);
        let asc = super::postfix_op_to_compute(
            &session,
            &state,
            &[],
            &plasm_core::expr_parser::PlasmPostfixOp::Sort {
                args: "score, ascending".into(),
            },
            "src",
            "id",
            "expr",
        )
        .expect("asc");
        match asc.source {
            super::DagNodeSource::Compute {
                op: crate::plasm_plan::ComputeOp::Sort { descending, .. },
                ..
            } => assert!(!descending),
            _ => panic!("expected compute sort"),
        }
        let desc = super::postfix_op_to_compute(
            &session,
            &state,
            &[],
            &plasm_core::expr_parser::PlasmPostfixOp::Sort {
                args: "score, descending".into(),
            },
            "src",
            "id",
            "expr",
        )
        .expect("desc");
        match desc.source {
            super::DagNodeSource::Compute {
                op: crate::plasm_plan::ComputeOp::Sort { descending, .. },
                ..
            } => assert!(descending),
            _ => panic!("expected compute sort"),
        }
    }

    /// teaching table `p#` indices are session-global; mixing another entity's symbols into a Commit projection
    /// must fail at compile time instead of producing all-null columns at runtime.
    #[test]
    fn postfix_projection_rejects_foreign_entity_domain_symbols() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_repo = map.ident_sym_entity_field_for("github", "Repository", "open_issues_count");
        let p_sha = map.ident_sym_entity_field_for("langmatrix", "Commit", "sha");
        let source = format!(
            r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits.limit(20)
commits[{p_repo},{p_sha}]
commits"#,
            p_repo = p_repo,
            p_sha = p_sha,
        );
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "foreign-field-proj",
            &source,
        )
        .expect_err("cross-entity symbols must not compile");
        assert!(
            (err.contains("open_issues_count") || err.contains("not a row symbol"))
                && (err.contains("not a row field")
                    || err.contains("null columns")
                    || err.contains("not a row symbol")
                    || err.contains("expected entity field")),
            "{err}"
        );
    }

    mod projection_props {
        use super::*;
        use proptest::prelude::*;
        use std::sync::OnceLock;

        fn github_session_cached() -> &'static ExecuteSession {
            static CELL: OnceLock<ExecuteSession> = OnceLock::new();
            CELL.get_or_init(github_repository_commit_session)
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(48))]

            #[test]
            fn nonempty_commit_field_subset_always_compiles(
                picks in prop::collection::vec(0usize..4usize, 1usize..=8usize)
            ) {
                let fields = ["sha", "message", "stats_additions", "total_changes"];
                let mut set = BTreeSet::new();
                for i in picks {
                    set.insert(fields[i % fields.len()]);
                }
                let proj = set.into_iter().collect::<Vec<_>>().join(",");
                let session = github_session_cached();
                let source = format!(
                    r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits.limit(3)
commits[{proj}]
commits"#
                );
                compile_plasm_dag_to_plan(
                    &PromptPipelineConfig::default(),
                    None,
                    session,
                    "prop-commit-subset",
                    &source,
                )
                .expect("projection");
            }

            #[test]
            fn repository_field_name_literal_in_commit_projection_fails(
                bad in "(open_issues_count|forks_count|stargazers_count)"
            ) {
                let session = github_session_cached();
                let source = format!(
                    r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits.limit(3)
commits[{bad}]
commits"#
                );
                let err = compile_plasm_dag_to_plan(
                    &PromptPipelineConfig::default(),
                    None,
                    session,
                    "prop-bad-commit-path",
                    &source,
                )
                .expect_err("reject");
                prop_assert!(
                    err.contains("null columns") || err.contains("not a row field"),
                    "{err}"
                );
            }
        }
    }

    #[test]
    fn matrix_bind_relation_hop_summary_relation_ir() {
        let session = test_session();
        let source = r#"item = LangItem("i1")
summary = item.summary
summary"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "matrix-bind-hop",
            source,
        )
        .expect("compile");
        let summary = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "summary")
            .expect("summary node");
        assert_eq!(summary["kind"], "relation");
        let ir = summary["relation"]["ir"]["expr"].to_string();
        assert!(
            ir.contains("LangItem") && ir.contains("summary"),
            "expected surface chain IR, got {ir}"
        );
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(dry.node_results.len(), 2, "{dry:?}");
    }

    #[test]
    fn matrix_bind_relation_hop_detail_relation_ir() {
        let session = test_session();
        let source = r#"item = LangItem("i1")
summary = item.summary
detail = summary.detail
detail"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "matrix-bind-hop-detail",
            source,
        )
        .expect("compile");
        let detail = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "detail")
            .expect("detail node");
        assert_eq!(detail["kind"], "relation");
        let ir = detail["relation"]["ir"]["expr"].to_string();
        assert!(
            ir.contains("__plasm_hole") && ir.contains("node_input") && ir.contains("LangSummary"),
            "expected row-hole relation IR from relation-sourced binding, got {ir}"
        );
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(dry.node_results.len(), 3, "{dry:?}");
    }

    #[test]
    fn for_each_heredoc_row_cursor_does_not_depend_on_underscore() {
        let session = test_session();
        let source = r#"items = LangItem.limit(2)
created = items => LangItem.create(title=<<T
row ${_.title}
T
)
created"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "for-each-row-heredoc",
            source,
        )
        .expect("compile");
        let created = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "created")
            .expect("for_each node");
        assert_eq!(created["kind"], "for_each");
        let depends = created["depends_on"].as_array().expect("depends_on");
        assert!(
            depends.iter().all(|d| d.as_str() != Some("_")),
            "depends_on must not include row cursor: {depends:?}"
        );
        let plan_value = crate::plasm_plan::parse_plan_value(&plan).expect("parse plan");
        crate::plasm_plan::validate_plan_artifact(&plan_value).expect("validate plan");
    }

    #[test]
    fn for_each_heredoc_cross_binding_collects_upstream_node() {
        let session = test_session();
        let source = r#"report = <<RPT
static body
RPT
items = LangItem.limit(2)
created = items => LangItem.create(title=<<T
${report.content}
T
)
created"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "for-each-cross-binding",
            source,
        )
        .expect("compile");
        let created = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "created")
            .expect("for_each node");
        let uses = created["uses_result"].as_array().expect("uses_result");
        assert!(
            uses.iter().any(|u| u["node"] == "report"),
            "cross-binding heredoc must record upstream node: {uses:?}"
        );
        let plan_value = crate::plasm_plan::parse_plan_value(&plan).expect("parse plan");
        crate::plasm_plan::validate_plan_artifact(&plan_value).expect("validate plan");
    }

    #[test]
    fn cross_binding_render_compiles_matrix_program() {
        let session = test_session();
        let source = r#"a = LangItem("i1")[id,title]
b = LangItem("i2")[id,title]
report = a,b <<MD
Pair: {{ a.id }} / {{ b.id }}
MD
report"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "cross-binding-render",
            source,
        )
        .expect("compile");
        let report = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "report")
            .expect("render node");
        assert_eq!(report["kind"], "compute");
        let uses = report["uses_result"].as_array().expect("uses_result");
        assert!(
            uses.iter().any(|u| u["node"] == "a"),
            "expected primary source in uses_result: {uses:?}"
        );
        assert!(
            uses.iter().any(|u| u["node"] == "b"),
            "expected cross-binding source in uses_result: {uses:?}"
        );
        let op = &report["compute"]["op"];
        assert_eq!(op["render_bindings"].as_array().map(|a| a.len()), Some(2));
        let plan_value = crate::plasm_plan::parse_plan_value(&plan).expect("parse plan");
        crate::plasm_plan::validate_plan_artifact(&plan_value).expect("validate plan");
    }

    #[test]
    fn lang_for_each_update_matrix_program_compiles_for_each_action() {
        let session = test_session();
        let source = "items = LangItem(\"i1\")[id,title,owner]\n\
            sync = items => LangItem(\"i1\").update(score=3, title=_.title, owner=_.owner)\n\
            sync";
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "lang_for_each_update",
            source,
        )
        .expect("compile");
        let sync = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "sync")
            .expect("for_each sync node");
        assert_eq!(sync["kind"], "for_each");
        assert_eq!(
            sync.pointer("/effect_template/kind")
                .and_then(|k| k.as_str()),
            Some("action")
        );
        let dry = crate::plasm_plan_run::evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert!(
            dry.node_results.iter().any(|nr| {
                nr.get("kind").and_then(|k| k.as_str()) == Some("for_each")
                    && nr.pointer("/effect_template/kind").and_then(|k| k.as_str())
                        == Some("action")
            }),
            "dry node_results: {:?}",
            dry.node_results
        );
    }

    #[test]
    fn binding_field_projection_root_rewrites_paren_to_bracket() {
        let session = test_session();
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "binding-projection-root",
            "rows = LangItem\npick = rows.limit(1)\npick(id, title)",
        )
        .expect("compile binding projection root");
        let ret = plan["return"].pointer("/node").and_then(|v| v.as_str());
        assert_eq!(
            ret,
            Some("return_1"),
            "paren root desugars to bracket projection return"
        );
    }

    #[test]
    fn rewrite_binding_field_projection_root_unit() {
        let session = test_session();
        let pipeline = PromptPipelineConfig::default();
        let mut state = CompileState::new(&pipeline, None);
        for node in compile_node_expr(&session, &state, "rows", "LangItem").expect("rows") {
            state.insert(node).expect("insert rows");
        }
        for node in compile_node_expr(&session, &state, "pick", "rows.limit(1)").expect("pick") {
            state.insert(node).expect("insert pick");
        }
        let rewritten = rewrite_binding_field_projection_root("pick(id, title)", &state);
        assert_eq!(rewritten.as_deref(), Some("pick[id, title]"));
    }

    #[test]
    fn invoke_rejects_unbound_phrase_ident_on_string_param() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "invoke-unknown-binding",
            r#"item = LangItem("i1")
bad = item.update(title=body)
bad"#,
        )
        .expect_err("unbound `body` must not plan as literal string");
        assert!(err.contains("body"), "{err}");
        assert!(
            err.contains("unknown program binding") || err.contains("program binding"),
            "{err}"
        );
    }

    #[test]
    fn invoke_accepts_bound_label_as_node_input_ref() {
        let session = test_session();
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "invoke-bound-label",
            r#"body = <<B
patch
B
item = LangItem("i1")
updated = item.update(title=body)
updated"#,
        )
        .expect("bound label lowers to node_input");
        let updated = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "updated")
            .expect("updated node");
        let uses = updated["uses_result"].as_array().expect("uses_result");
        assert!(
            uses.iter().any(|u| u["node"] == "body"),
            "title=body must reference binding: {uses:?}"
        );
    }
