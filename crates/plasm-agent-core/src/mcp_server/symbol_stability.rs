//! Symbol stability regression tests (language matrix, append-only `m#` invariants).

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::execute_session::ExecuteSession;
    use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
    use crate::http_execute::{apply_capability_seeds, CapabilitySeed};
    use crate::plasm_compile::compile_plasm_expression;
    use crate::plasm_plan_run::{evaluate_plasm_comp_dry, format_session_symbolic_parse_error};
    use crate::server_state::PlasmHostState;
    use indexmap::IndexMap;
    use plasm_core::discovery::CgsRegistry;
    use plasm_core::loader::load_schema_dir;
    use plasm_core::symbol_map_fingerprint_hex;
    use plasm_core::{CgsContext, TeachingExposureSession};
    use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
    use uuid::Uuid;

    use crate::test_support::block_on_worker_stack;

    const ENTRY: &str = "langmatrix";

    fn matrix_fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix")
    }

    fn matrix_host() -> Option<PlasmHostState> {
        let dir = matrix_fixture_dir();
        if !dir.is_dir() {
            return None;
        }
        let cgs = Arc::new(load_schema_dir(&dir).ok()?);
        let reg = CgsRegistry::from_pairs(vec![(
            ENTRY.into(),
            "Langmatrix".into(),
            vec![ENTRY.into()],
            cgs.clone(),
        )]);
        let engine = ExecutionEngine::new(ExecutionConfig::default()).ok()?;
        Some(build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry: Arc::new(reg),
            catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        }))
    }

    fn item_tag_branch_workflow_seeds() -> Vec<CapabilitySeed> {
        vec![
            CapabilitySeed {
                entry_id: ENTRY.into(),
                entity: "LangItem".into(),
            },
            CapabilitySeed {
                entry_id: ENTRY.into(),
                entity: "LangTag".into(),
            },
            CapabilitySeed {
                entry_id: ENTRY.into(),
                entity: "CompoundBranch".into(),
            },
        ]
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SymbolSnapshot {
        fingerprint: String,
        domain_revision: u32,
        create_m: String,
        m_token_capability: Option<(String, String)>,
    }

    fn snapshot_session(es: &ExecuteSession, m_token: &str) -> SymbolSnapshot {
        let exp = es
            .teaching_exposure
            .as_ref()
            .expect("teaching_exposure required for symbol stability tests");
        let map = exp.symbol_map_arc();
        let create_m = map.method_sym_for(ENTRY, "LangItem", "langitem_create");
        let m_token_capability = map
            .resolve_method_symbol_triple(m_token)
            .map(|(entry, domain, cap)| (format!("{entry}.{domain}.{cap}"), cap.to_string()));
        SymbolSnapshot {
            fingerprint: symbol_map_fingerprint_hex(exp),
            domain_revision: es.domain_revision,
            create_m,
            m_token_capability,
        }
    }

    fn langitem_create_program(exp: &TeachingExposureSession) -> String {
        let map = exp.symbol_map_arc();
        let e = map.entity_sym_for(ENTRY, "LangItem");
        let m = map.method_sym_for(ENTRY, "LangItem", "langitem_create");
        let p_title = map.ident_sym_cap_param_for(ENTRY, "LangItem", "langitem_create", "title");
        let p_owner = map.ident_sym_cap_param_for(ENTRY, "LangItem", "langitem_create", "owner");
        let p_score = map.ident_sym_cap_param_for(ENTRY, "LangItem", "langitem_create", "score");
        let p_tags = map.ident_sym_cap_param_for(ENTRY, "LangItem", "langitem_create", "tags");
        format!(
            r#"created = {e}.{m}({p_title}="Document tags", {p_owner}="alice", {p_score}=1, {p_tags}=["bug", "docs"])
created"#
        )
    }

    fn create_binding(exp: &TeachingExposureSession) -> (String, String) {
        let map = exp.symbol_map_arc();
        let m = map.method_sym_for(ENTRY, "LangItem", "langitem_create");
        let cap = map
            .resolve_method_symbol_triple(m.as_str())
            .expect("langitem_create m resolves")
            .2
            .to_string();
        (m, cap)
    }

    fn assert_surface_capability(
        dry: &crate::plasm_plan_run::DryPlasmPlanEvaluation,
        es: &ExecuteSession,
        _cap: &str,
    ) {
        let text =
            crate::plasm_plan_run::render_plasm_plan_dry_text_for_session(dry, None, Some(es));
        assert!(
            text.contains("langitem_create") || text.contains("create e"),
            "expected langitem_create (or create-kind) in dry plan:\n{text}"
        );
    }

    fn append_symbol_stability_context_for_test(
        session: &ExecuteSession,
        message: &str,
        source_line: &str,
    ) -> String {
        crate::plasm_plan_run::format_session_symbolic_parse_error(
            session,
            None,
            &plasm_core::PromptPipelineConfig::default(),
            source_line,
            &plasm_core::expr_parser::ParseError {
                kind: plasm_core::expr_parser::ParseErrorKind::Other {
                    message: message.to_string(),
                },
                offset: 0,
            },
        )
    }

    fn compile_dry(
        st: &PlasmHostState,
        es: &ExecuteSession,
        tag: &str,
        program: &str,
    ) -> crate::plasm_plan_run::DryPlasmPlanEvaluation {
        let pipeline = st.engine.prompt_pipeline();
        let cross = st.sessions.symbol_map_cross_cache();
        let bundle = compile_plasm_expression(pipeline, Some(cross), es, tag, program)
            .unwrap_or_else(|e| panic!("compile `{tag}`: {e}"));
        evaluate_plasm_comp_dry(es, &bundle).expect("dry-run")
    }

    /// Mirrors reported workflow: open → create dry-run → intermediate plasm calls → reuse mutator `m#`.
    #[test]
    fn symbol_stability_langitem_create_survives_intermediate_plasm_reads() {
        block_on_worker_stack(
            symbol_stability_langitem_create_survives_intermediate_plasm_reads_inner,
        );
    }

    async fn symbol_stability_langitem_create_survives_intermediate_plasm_reads_inner() {
        let Some(st) = matrix_host() else {
            return;
        };
        let st = Arc::new(st);
        let logical_id = Uuid::new_v4();
        let intent = "create a lang item with title and tags and list tags";

        let out = apply_capability_seeds(
            st.as_ref(),
            None,
            None,
            item_tag_branch_workflow_seeds(),
            None,
            None,
            Some(logical_id),
            intent,
        )
        .await
        .expect("plasm_context open");

        let es = st
            .get_execute_session(&out.prompt_hash, &out.session_id)
            .await
            .expect("execute session");
        let exp = es.teaching_exposure.as_ref().expect("exposure");
        let map = exp.symbol_map_arc();
        let m_create = map.method_sym_for(ENTRY, "LangItem", "langitem_create");
        let binding_after_open = create_binding(exp);

        let snap0 = snapshot_session(&es, &m_create);
        assert_eq!(
            snap0.m_token_capability.as_ref().map(|(_, c)| c.as_str()),
            Some("langitem_create"),
            "create m# must resolve to langitem_create at open"
        );

        let create_program = langitem_create_program(exp);
        let create_dry = compile_dry(st.as_ref(), &es, "langitem_create", &create_program);
        assert_surface_capability(&create_dry, &es, "langitem_create");

        let snap1 = snapshot_session(&es, &m_create);
        assert_eq!(snap0.fingerprint, snap1.fingerprint);
        assert_eq!(snap0.create_m, snap1.create_m);
        assert_eq!(binding_after_open, create_binding(exp));

        let _item_read = compile_dry(
            st.as_ref(),
            &es,
            "item_read_between",
            &langitem_create_program(exp),
        );

        let snap2 = snapshot_session(&es, &m_create);
        assert_eq!(
            snap0.fingerprint, snap2.fingerprint,
            "symbol_map_fingerprint must not change across intermediate plasm reads"
        );
        assert_eq!(
            snap0.m_token_capability, snap2.m_token_capability,
            "m# binding for langitem_create must remain stable"
        );
        assert_eq!(binding_after_open, create_binding(exp));

        let create_dry_again =
            compile_dry(st.as_ref(), &es, "langitem_create_retry", &create_program);
        assert_surface_capability(&create_dry_again, &es, "langitem_create");

        let err = compile_plasm_expression(
            st.engine.prompt_pipeline(),
            Some(st.sessions.symbol_map_cross_cache()),
            &es,
            "wrong_mutator",
            &create_program,
        );
        assert!(err.is_ok(), "same m# token must still compile as mutator");
    }

    /// Intent-scored expand may add langitem_query append-only; existing create `m#` must not move.
    #[test]
    fn symbol_stability_extend_intent_adds_query_without_reassigning_create_m() {
        block_on_worker_stack(
            symbol_stability_extend_intent_adds_query_without_reassigning_create_m_inner,
        );
    }

    async fn symbol_stability_extend_intent_adds_query_without_reassigning_create_m_inner() {
        let Some(st) = matrix_host() else {
            return;
        };
        let st = Arc::new(st);
        let logical_id = Uuid::new_v4();
        let seeds = item_tag_branch_workflow_seeds();

        let out = apply_capability_seeds(
            st.as_ref(),
            None,
            None,
            seeds.clone(),
            None,
            None,
            Some(logical_id),
            "create a lang item with title",
        )
        .await
        .expect("open");

        let es = st
            .get_execute_session(&out.prompt_hash, &out.session_id)
            .await
            .expect("session");
        let snap_before = snapshot_session(
            &es,
            &es.teaching_exposure
                .as_ref()
                .expect("exposure")
                .symbol_map_arc()
                .method_sym_for(ENTRY, "LangItem", "langitem_create"),
        );

        let out_extend = apply_capability_seeds(
            st.as_ref(),
            None,
            Some((out.prompt_hash.as_str(), out.session_id.as_str())),
            seeds,
            None,
            None,
            Some(logical_id),
            "list language items and langitem query",
        )
        .await
        .expect("extend");

        let es2 = st
            .get_execute_session(&out_extend.prompt_hash, &out_extend.session_id)
            .await
            .expect("session after extend");
        let m_create = es2
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc()
            .method_sym_for(ENTRY, "LangItem", "langitem_create");
        let snap_after = snapshot_session(&es2, &m_create);

        assert_eq!(
            snap_before.create_m, snap_after.create_m,
            "langitem_create m# must not shift on intent-scored extend"
        );
        assert_eq!(
            snap_before.m_token_capability, snap_after.m_token_capability,
            "langitem_create capability binding must be unchanged"
        );

        let exp = es2.teaching_exposure.as_ref().expect("exposure");
        let query_m = exp
            .symbol_map_arc()
            .method_sym_for(ENTRY, "LangItem", "langitem_query");
        if query_m.starts_with('m') {
            let map = exp.symbol_map_arc();
            let triple = map
                .resolve_method_symbol_triple(query_m.as_str())
                .expect("langitem_query m resolves");
            assert_eq!(triple.2, "langitem_query");
            assert_ne!(
                query_m, snap_after.create_m,
                "query cap must not steal langitem_create slot"
            );
        }
    }

    /// Fresh logical session may assign different m# (wave-structure); document cross-session hazard.
    #[test]
    fn symbol_stability_new_session_may_differ_but_ledger_restores_numbering() {
        block_on_worker_stack(
            symbol_stability_new_session_may_differ_but_ledger_restores_numbering_inner,
        );
    }

    async fn symbol_stability_new_session_may_differ_but_ledger_restores_numbering_inner() {
        let Some(st) = matrix_host() else {
            return;
        };
        let st = Arc::new(st);
        let seeds = item_tag_branch_workflow_seeds();
        let intent = "lang item tag documentation workflow";

        let out_a = apply_capability_seeds(
            st.as_ref(),
            None,
            None,
            seeds.clone(),
            None,
            None,
            Some(Uuid::new_v4()),
            intent,
        )
        .await
        .expect("session a");

        let es_a = st
            .get_execute_session(&out_a.prompt_hash, &out_a.session_id)
            .await
            .expect("session a row");
        let m_a = es_a
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc()
            .method_sym_for(ENTRY, "LangItem", "langitem_create");

        let logical_b = Uuid::new_v4();
        let out_b = apply_capability_seeds(
            st.as_ref(),
            None,
            None,
            seeds.clone(),
            None,
            None,
            Some(logical_b),
            intent,
        )
        .await
        .expect("session b");

        let es_b = st
            .get_execute_session(&out_b.prompt_hash, &out_b.session_id)
            .await
            .expect("session b row");
        let m_b = es_b
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc()
            .method_sym_for(ENTRY, "LangItem", "langitem_create");

        for (label, m_sym, es) in [("a", m_a.as_str(), &es_a), ("b", m_b.as_str(), &es_b)] {
            let map = es
                .teaching_exposure
                .as_ref()
                .expect("exposure")
                .symbol_map_arc();
            let triple = map
                .resolve_method_symbol_triple(m_sym)
                .unwrap_or_else(|| panic!("session {label} {m_sym} must resolve"));
            assert_eq!(triple.2, "langitem_create");
        }

        let out_b_extend = apply_capability_seeds(
            st.as_ref(),
            None,
            Some((out_b.prompt_hash.as_str(), out_b.session_id.as_str())),
            seeds,
            None,
            None,
            Some(logical_b),
            "continue lang item tag workflow",
        )
        .await
        .expect("extend b");

        let es_b2 = st
            .get_execute_session(&out_b_extend.prompt_hash, &out_b_extend.session_id)
            .await
            .expect("session b after extend");
        let m_b2 = es_b2
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc()
            .method_sym_for(ENTRY, "LangItem", "langitem_create");
        assert_eq!(
            m_b, m_b2,
            "extend on same logical session must preserve langitem_create m#"
        );
    }

    #[test]
    fn symbol_stability_mutator_query_mismatch_includes_fingerprint_in_parse_error() {
        block_on_worker_stack(
            symbol_stability_mutator_query_mismatch_includes_fingerprint_in_parse_error_inner,
        );
    }

    async fn symbol_stability_mutator_query_mismatch_includes_fingerprint_in_parse_error_inner() {
        let Some(st) = matrix_host() else {
            return;
        };
        let st = Arc::new(st);
        let out = apply_capability_seeds(
            st.as_ref(),
            None,
            None,
            vec![CapabilitySeed {
                entry_id: ENTRY.into(),
                entity: "LangItem".into(),
            }],
            None,
            None,
            Some(Uuid::new_v4()),
            "list language items",
        )
        .await
        .expect("open");

        let es = st
            .get_execute_session(&out.prompt_hash, &out.session_id)
            .await
            .expect("session");
        let exp = es.teaching_exposure.as_ref().expect("exposure");
        let map = exp.symbol_map_arc();
        let m_query = map.method_sym_for(ENTRY, "LangItem", "langitem_query");
        if !m_query.starts_with('m') {
            return;
        }

        let m_create = map.method_sym_for(ENTRY, "LangItem", "langitem_create");
        let line = langitem_create_program(exp).replace(&m_create, &m_query);
        let err = compile_plasm_expression(
            st.engine.prompt_pipeline(),
            Some(st.sessions.symbol_map_cross_cache()),
            &es,
            "query_as_mutator",
            &line,
        )
        .expect_err("query m# in mutator invoke position")
        .to_string();
        let msg = if err.contains("not a mutator") || err.contains("is query") {
            append_symbol_stability_context_for_test(&es, &err, &line)
        } else {
            format_session_symbolic_parse_error(
                &es,
                Some(st.sessions.symbol_map_cross_cache()),
                st.engine.prompt_pipeline(),
                &line,
                &plasm_core::expr_parser::ParseError {
                    kind: plasm_core::expr_parser::ParseErrorKind::Other {
                        message: err.clone(),
                    },
                    offset: 0,
                },
            )
        };
        assert!(
            msg.contains("symbol_map_fingerprint="),
            "parse error must include fingerprint: {msg}"
        );
        assert!(
            msg.contains("domain_revision="),
            "parse error must include domain_revision: {msg}"
        );
        assert!(
            msg.contains(&m_query) && msg.contains("langitem_query"),
            "parse error must name resolved binding: {msg}"
        );
    }

    fn item_tag_line_seeds() -> Vec<CapabilitySeed> {
        vec![
            CapabilitySeed {
                entry_id: ENTRY.into(),
                entity: "LangItem".into(),
            },
            CapabilitySeed {
                entry_id: ENTRY.into(),
                entity: "LangTag".into(),
            },
            CapabilitySeed {
                entry_id: ENTRY.into(),
                entity: "LangLine".into(),
            },
        ]
    }

    /// Open LangItem+LangTag+LangLine, extend CompoundBranch — create `m#` must not move.
    #[test]
    fn symbol_stability_item_tag_line_extend_branch_preserves_create_m() {
        block_on_worker_stack(
            symbol_stability_item_tag_line_extend_branch_preserves_create_m_inner,
        );
    }

    async fn symbol_stability_item_tag_line_extend_branch_preserves_create_m_inner() {
        let Some(st) = matrix_host() else {
            return;
        };
        let st = Arc::new(st);
        let logical_id = Uuid::new_v4();
        let intent = "create a lang item and manage tags";

        let seeds_open = item_tag_line_seeds();
        let out = apply_capability_seeds(
            st.as_ref(),
            None,
            None,
            seeds_open.clone(),
            None,
            None,
            Some(logical_id),
            intent,
        )
        .await
        .expect("open");

        let es = st
            .get_execute_session(&out.prompt_hash, &out.session_id)
            .await
            .expect("session");
        let m_before = es
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc()
            .method_sym_for(ENTRY, "LangItem", "langitem_create");

        let mut seeds_extend = seeds_open;
        seeds_extend.push(CapabilitySeed {
            entry_id: ENTRY.into(),
            entity: "CompoundBranch".into(),
        });
        let out_extend = apply_capability_seeds(
            st.as_ref(),
            None,
            Some((out.prompt_hash.as_str(), out.session_id.as_str())),
            seeds_extend,
            None,
            None,
            Some(logical_id),
            intent,
        )
        .await
        .expect("extend branch");

        let es2 = st
            .get_execute_session(&out_extend.prompt_hash, &out_extend.session_id)
            .await
            .expect("session after extend");
        let m_after = es2
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc()
            .method_sym_for(ENTRY, "LangItem", "langitem_create");

        assert_eq!(
            m_before, m_after,
            "langitem_create m# must stay stable when CompoundBranch is added on extend"
        );
        let program = langitem_create_program(es2.teaching_exposure.as_ref().expect("exp"));
        let _ = compile_plasm_expression(
            st.engine.prompt_pipeline(),
            Some(st.sessions.symbol_map_cross_cache()),
            &es2,
            "create_after_extend",
            &program,
        )
        .expect("langitem_create program must compile after CompoundBranch extend");
    }

    #[test]
    fn symbol_stability_compile_preserves_exposure_fingerprint() {
        block_on_worker_stack(symbol_stability_compile_preserves_exposure_fingerprint_inner);
    }

    async fn symbol_stability_compile_preserves_exposure_fingerprint_inner() {
        let dir = matrix_fixture_dir();
        if !dir.is_dir() {
            return;
        }
        let cgs = Arc::new(load_schema_dir(&dir).expect("plasm_language_matrix"));
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            ENTRY.into(),
            Arc::new(CgsContext::entry(ENTRY, cgs.clone())),
        );
        let exp = TeachingExposureSession::new(cgs.as_ref(), ENTRY, &["LangItem", "LangTag"]);
        let fp_before = symbol_map_fingerprint_hex(&exp);
        let es = ExecuteSession::new(
            "ph".into(),
            "sid".into(),
            cgs.clone(),
            ctxs,
            ENTRY.into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into(), "LangTag".into()],
            Some(exp.clone()),
            None,
            cgs.catalog_cgs_hash_hex(),
            Some("lang item workflow".into()),
        );
        let cross = plasm_core::SymbolMapCrossRequestCache::new(8);
        let program = langitem_create_program(&exp);
        let _ = compile_plasm_expression(
            &plasm_core::PromptPipelineConfig::default(),
            Some(&cross),
            &es,
            "create",
            &program,
        )
        .expect("langitem_create compiles against teaching exposure");
        let fp_after = symbol_map_fingerprint_hex(es.teaching_exposure.as_ref().expect("exp"));
        assert_eq!(fp_before, fp_after);
    }
}
