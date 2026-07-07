//! Symbol stability regression tests (GitHub catalog, append-only `m#` invariants).

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::execute_session::ExecuteSession;
    use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
    use crate::http_execute::{apply_capability_seeds, CapabilitySeed, RankedCapabilitiesArg};
    use crate::plasm_compile::compile_plasm_expression;
    use crate::plasm_plan_run::{evaluate_plasm_comp_dry, format_session_symbolic_parse_error};
    use crate::server_state::PlasmHostState;
    use indexmap::IndexMap;
    use plasm_core::discovery::InMemoryCgsRegistry;
    use plasm_core::loader::load_schema_dir;
    use plasm_core::symbol_map_fingerprint_hex;
    use plasm_core::{CgsContext, TeachingExposureSession};
    use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
    use uuid::Uuid;

    fn github_fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/github")
    }

    fn github_host() -> Option<PlasmHostState> {
        let dir = github_fixture_dir();
        if !dir.is_dir() {
            return None;
        }
        let cgs = Arc::new(load_schema_dir(&dir).ok()?);
        let reg = InMemoryCgsRegistry::from_pairs(vec![(
            "github".into(),
            "GitHub".into(),
            vec!["github".into()],
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

    fn label_branch_workflow_seeds() -> Vec<CapabilitySeed> {
        vec![
            CapabilitySeed {
                entry_id: "github".into(),
                entity: "Repository".into(),
            },
            CapabilitySeed {
                entry_id: "github".into(),
                entity: "Issue".into(),
            },
            CapabilitySeed {
                entry_id: "github".into(),
                entity: "Branch".into(),
            },
        ]
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SymbolSnapshot {
        fingerprint: String,
        domain_revision: u32,
        branch_create_m: String,
        m_token_capability: Option<(String, String)>,
    }

    fn snapshot_session(es: &ExecuteSession, m_token: &str) -> SymbolSnapshot {
        let exp = es
            .teaching_exposure
            .as_ref()
            .expect("teaching_exposure required for symbol stability tests");
        let map = exp.symbol_map_arc();
        let branch_create_m = map.method_sym_for("github", "Repository", "repo_branch_create");
        let m_token_capability = map
            .resolve_method_symbol_triple(m_token)
            .map(|(entry, domain, cap)| (format!("{entry}.{domain}.{cap}"), cap.to_string()));
        SymbolSnapshot {
            fingerprint: symbol_map_fingerprint_hex(exp),
            domain_revision: es.domain_revision,
            branch_create_m,
            m_token_capability,
        }
    }

    fn github_branch_create_program(exp: &TeachingExposureSession) -> String {
        let map = exp.symbol_map_arc();
        let e = map.entity_sym_for("github", "Repository");
        let owner = map.ident_sym_entity_field_for("github", "Repository", "owner");
        let repo = map.ident_sym_entity_field_for("github", "Repository", "repo");
        let m = map.method_sym_for("github", "Repository", "repo_branch_create");
        let p_name =
            map.ident_sym_cap_param_for("github", "Repository", "repo_branch_create", "name");
        let p_sha =
            map.ident_sym_cap_param_for("github", "Repository", "repo_branch_create", "sha");
        format!(
            "{e}({owner}=\"o\", {repo}=\"r\").{m}({p_name}=\"feat/label-color-guide\", {p_sha}=\"deadbeef\")"
        )
    }

    fn branch_create_binding(exp: &TeachingExposureSession) -> (String, String) {
        let map = exp.symbol_map_arc();
        let m = map.method_sym_for("github", "Repository", "repo_branch_create");
        let cap = map
            .resolve_method_symbol_triple(m.as_str())
            .expect("branch-create m resolves")
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
            text.contains("branch-create") || text.contains("repo_branch_create"),
            "expected branch-create capability in dry plan:\n{text}"
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

    /// Mirrors reported workflow: open → branch-create dry-run → read-only plasm calls → reuse mutator `m#`.
    #[tokio::test]
    async fn symbol_stability_github_branch_create_survives_intermediate_plasm_reads() {
        let Some(st) = github_host() else {
            return;
        };
        let st = Arc::new(st);
        let logical_id = Uuid::new_v4();
        let intent = "create branch feat/label-color-guide from commit sha and list issues";

        let out = apply_capability_seeds(
            st.as_ref(),
            None,
            None,
            label_branch_workflow_seeds(),
            None,
            None,
            Some(logical_id),
            intent,
            RankedCapabilitiesArg::Unspecified,
        )
        .await
        .expect("plasm_context open");

        let es = st
            .get_execute_session(&out.prompt_hash, &out.session_id)
            .await
            .expect("execute session");
        let exp = es.teaching_exposure.as_ref().expect("exposure");
        let map = exp.symbol_map_arc();
        let m_branch = map.method_sym_for("github", "Repository", "repo_branch_create");
        let binding_after_open = branch_create_binding(exp);

        let snap0 = snapshot_session(&es, &m_branch);
        assert_eq!(
            snap0.m_token_capability.as_ref().map(|(_, c)| c.as_str()),
            Some("repo_branch_create"),
            "branch-create m# must resolve to repo_branch_create at open"
        );

        let branch_program = github_branch_create_program(exp);
        let branch_dry = compile_dry(st.as_ref(), &es, "branch_create", &branch_program);
        assert_surface_capability(&branch_dry, &es, "repo_branch_create");

        let snap1 = snapshot_session(&es, &m_branch);
        assert_eq!(snap0.fingerprint, snap1.fingerprint);
        assert_eq!(snap0.branch_create_m, snap1.branch_create_m);
        assert_eq!(binding_after_open, branch_create_binding(exp));

        // Intermediate plasm compiles (pc4/pc5 analog) — no plasm_context between.
        let _repo_read = compile_dry(
            st.as_ref(),
            &es,
            "repo_read_between",
            &github_branch_create_program(exp),
        );

        let snap2 = snapshot_session(&es, &m_branch);
        assert_eq!(
            snap0.fingerprint, snap2.fingerprint,
            "symbol_map_fingerprint must not change across intermediate plasm reads"
        );
        assert_eq!(
            snap0.m_token_capability, snap2.m_token_capability,
            "m# binding for branch-create must remain stable"
        );
        assert_eq!(binding_after_open, branch_create_binding(exp));

        let branch_dry_again =
            compile_dry(st.as_ref(), &es, "branch_create_retry", &branch_program);
        assert_surface_capability(&branch_dry_again, &es, "repo_branch_create");

        let err = compile_plasm_expression(
            st.engine.prompt_pipeline(),
            Some(st.sessions.symbol_map_cross_cache()),
            &es,
            "wrong_mutator",
            &branch_program,
        );
        assert!(err.is_ok(), "same m# token must still compile as mutator");
    }

    /// Intent-scored expand adds org_public_repos_query append-only; existing branch-create m# must not move.
    #[tokio::test]
    async fn symbol_stability_extend_intent_adds_query_without_reassigning_branch_create_m() {
        let Some(st) = github_host() else {
            return;
        };
        let st = Arc::new(st);
        let logical_id = Uuid::new_v4();
        let seeds = label_branch_workflow_seeds();

        let out = apply_capability_seeds(
            st.as_ref(),
            None,
            None,
            seeds.clone(),
            None,
            None,
            Some(logical_id),
            "create branch for label guide",
            RankedCapabilitiesArg::Unspecified,
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
                .method_sym_for("github", "Repository", "repo_branch_create"),
        );

        let out_extend = apply_capability_seeds(
            st.as_ref(),
            None,
            Some((out.prompt_hash.as_str(), out.session_id.as_str())),
            seeds,
            None,
            None,
            Some(logical_id),
            "list public organization repositories and org repos query",
            RankedCapabilitiesArg::Unspecified,
        )
        .await
        .expect("extend");

        let es2 = st
            .get_execute_session(&out_extend.prompt_hash, &out_extend.session_id)
            .await
            .expect("session after extend");
        let m_branch = es2
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc()
            .method_sym_for("github", "Repository", "repo_branch_create");
        let snap_after = snapshot_session(&es2, &m_branch);

        assert_eq!(
            snap_before.branch_create_m, snap_after.branch_create_m,
            "branch-create m# must not shift on intent-scored extend"
        );
        assert_eq!(
            snap_before.m_token_capability, snap_after.m_token_capability,
            "branch-create capability binding must be unchanged"
        );

        let exp = es2.teaching_exposure.as_ref().expect("exposure");
        let org_query_m =
            exp.symbol_map_arc()
                .method_sym_for("github", "Repository", "org_public_repos_query");
        if org_query_m.starts_with('m') {
            let map = exp.symbol_map_arc();
            let triple = map
                .resolve_method_symbol_triple(org_query_m.as_str())
                .expect("org query m resolves");
            assert_eq!(triple.2, "org_public_repos_query");
            assert_ne!(
                org_query_m, snap_after.branch_create_m,
                "new query cap must not steal branch-create slot"
            );
        }
    }

    /// Fresh logical session may assign different m# (wave-structure); document cross-session hazard.
    #[tokio::test]
    async fn symbol_stability_new_session_may_differ_but_ledger_restores_numbering() {
        let Some(st) = github_host() else {
            return;
        };
        let st = Arc::new(st);
        let seeds = label_branch_workflow_seeds();
        let intent = "label documentation branch workflow";

        let out_a = apply_capability_seeds(
            st.as_ref(),
            None,
            None,
            seeds.clone(),
            None,
            None,
            Some(Uuid::new_v4()),
            intent,
            RankedCapabilitiesArg::Unspecified,
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
            .method_sym_for("github", "Repository", "repo_branch_create");

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
            RankedCapabilitiesArg::Unspecified,
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
            .method_sym_for("github", "Repository", "repo_branch_create");

        // Cross-session numbering may diverge (wave-structure); both must still resolve correctly.
        for (label, m_sym, es) in [("a", m_a.as_str(), &es_a), ("b", m_b.as_str(), &es_b)] {
            let map = es
                .teaching_exposure
                .as_ref()
                .expect("exposure")
                .symbol_map_arc();
            let triple = map
                .resolve_method_symbol_triple(m_sym)
                .unwrap_or_else(|| panic!("session {label} {m_sym} must resolve"));
            assert_eq!(triple.2, "repo_branch_create");
        }

        // Extend on session b with same logical id restores append-only ledger from first open.
        let out_b_extend = apply_capability_seeds(
            st.as_ref(),
            None,
            Some((out_b.prompt_hash.as_str(), out_b.session_id.as_str())),
            seeds,
            None,
            None,
            Some(logical_b),
            "continue label branch workflow",
            RankedCapabilitiesArg::Unspecified,
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
            .method_sym_for("github", "Repository", "repo_branch_create");
        assert_eq!(
            m_b, m_b2,
            "extend on same logical session must preserve branch-create m#"
        );
    }

    #[tokio::test]
    async fn symbol_stability_mutator_query_mismatch_includes_fingerprint_in_parse_error() {
        let Some(st) = github_host() else {
            return;
        };
        let st = Arc::new(st);
        let out = apply_capability_seeds(
            st.as_ref(),
            None,
            None,
            vec![CapabilitySeed {
                entry_id: "github".into(),
                entity: "Repository".into(),
            }],
            None,
            None,
            Some(Uuid::new_v4()),
            "list public organization repositories",
            RankedCapabilitiesArg::Unspecified,
        )
        .await
        .expect("open");

        let es = st
            .get_execute_session(&out.prompt_hash, &out.session_id)
            .await
            .expect("session");
        let exp = es.teaching_exposure.as_ref().expect("exposure");
        let map = exp.symbol_map_arc();
        let m_query = map.method_sym_for("github", "Repository", "org_public_repos_query");
        if !m_query.starts_with('m') {
            return;
        }

        let m_branch = map.method_sym_for("github", "Repository", "repo_branch_create");
        let line = github_branch_create_program(exp).replace(&m_branch, &m_query);
        let err = compile_plasm_expression(
            st.engine.prompt_pipeline(),
            Some(st.sessions.symbol_map_cross_cache()),
            &es,
            "query_as_mutator",
            &line,
        )
        .expect_err("query m# in mutator invoke position");
        let msg = if err.contains("not a mutator") {
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
            msg.contains(&m_query) && msg.contains("org_public_repos_query"),
            "parse error must name resolved binding: {msg}"
        );
    }

    fn repo_issue_label_seeds() -> Vec<CapabilitySeed> {
        vec![
            CapabilitySeed {
                entry_id: "github".into(),
                entity: "Repository".into(),
            },
            CapabilitySeed {
                entry_id: "github".into(),
                entity: "Issue".into(),
            },
            CapabilitySeed {
                entry_id: "github".into(),
                entity: "Label".into(),
            },
        ]
    }

    /// Reported workflow: open Repository+Issue+Label, extend Branch — branch-create `m#` must not move.
    #[tokio::test]
    async fn symbol_stability_repo_issue_label_extend_branch_preserves_branch_create_m() {
        let Some(st) = github_host() else {
            return;
        };
        let st = Arc::new(st);
        let logical_id = Uuid::new_v4();
        let intent = "create branch feat/label-color-guide and manage issue labels";

        let seeds_open = repo_issue_label_seeds();
        let out = apply_capability_seeds(
            st.as_ref(),
            None,
            None,
            seeds_open.clone(),
            None,
            None,
            Some(logical_id),
            intent,
            RankedCapabilitiesArg::Unspecified,
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
            .method_sym_for("github", "Repository", "repo_branch_create");

        let mut seeds_extend = seeds_open;
        seeds_extend.push(CapabilitySeed {
            entry_id: "github".into(),
            entity: "Branch".into(),
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
            RankedCapabilitiesArg::Unspecified,
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
            .method_sym_for("github", "Repository", "repo_branch_create");

        assert_eq!(
            m_before, m_after,
            "repo_branch_create m# must stay stable when Branch entity is added on extend"
        );
        let program = github_branch_create_program(es2.teaching_exposure.as_ref().expect("exp"));
        let _ = compile_plasm_expression(
            st.engine.prompt_pipeline(),
            Some(st.sessions.symbol_map_cross_cache()),
            &es2,
            "branch_after_extend",
            &program,
        )
        .expect("branch-create program must compile after Branch extend");
    }

    #[tokio::test]
    async fn symbol_stability_compile_preserves_exposure_fingerprint() {
        let dir = github_fixture_dir();
        if !dir.is_dir() {
            return;
        }
        let cgs = Arc::new(load_schema_dir(&dir).expect("github"));
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        let exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["Repository", "Issue"]);
        let fp_before = symbol_map_fingerprint_hex(&exp);
        let es = ExecuteSession::new(
            "ph".into(),
            "sid".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["Repository".into(), "Issue".into()],
            Some(exp.clone()),
            None,
            cgs.catalog_cgs_hash_hex(),
            Some("branch workflow".into()),
            None,
        );
        let cross = plasm_core::SymbolMapCrossRequestCache::new(8);
        let program = github_branch_create_program(&exp);
        let _ = compile_plasm_expression(
            &plasm_core::PromptPipelineConfig::default(),
            Some(&cross),
            &es,
            "branch",
            &program,
        )
        .expect("branch create compiles against teaching exposure");
        let fp_after = symbol_map_fingerprint_hex(es.teaching_exposure.as_ref().expect("exp"));
        assert_eq!(fp_before, fp_after);
    }
}
