use super::*;

struct Transport {
    calls: Arc<Mutex<Vec<String>>>,
    parents: usize,
    pause_on_child: Option<Arc<(tokio::sync::Notify, tokio::sync::Notify)>>,
    cancel_on_child: Option<crate::operation::ExecutionScope>,
}
#[async_trait]
impl HttpTransport for Transport {
    async fn send_compiled_http(
        &self,
        _: &str,
        req: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(Value, Option<String>), RuntimeError> {
        self.calls.lock().unwrap().push(req.path.clone());
        if req.path.ends_with("/tags") {
            if let Some(gate) = &self.pause_on_child {
                gate.0.notify_one();
                gate.1.notified().await;
            }
            if let Some(scope) = &self.cancel_on_child {
                scope.cancel();
            }
        }
        let parts: Vec<_> = req.path.trim_matches('/').split('/').collect();
        let rows = match parts.as_slice() {
            ["touch"] => vec![],
            ["items"] => (0..self.parents)
                .map(|i| json!({"id":format!("i{i}"), "title":format!("Title {i}"),"state":"open"}))
                .collect::<Vec<_>>(),
            ["items", id, "tags"] => {
                let n: usize = id.strip_prefix('i').unwrap().parse().unwrap();
                (0..n).map(|j| json!({"id":format!("{id}-t{j}"), "item_id":id,"label":format!("L{j}")})).collect()
            }
            _ => panic!("unexpected path {}", req.path),
        };
        Ok((json!(rows), None))
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(Value, Option<String>), RuntimeError> {
        panic!("unexpected absolute GET")
    }
}
pub(super) fn fixture(parents: usize) -> (ExecuteSession, PlasmHostState, Arc<Mutex<Vec<String>>>) {
    fixture_with_cancel(parents, None)
}
pub(super) fn fixture_with_cancel(
    parents: usize,
    cancel_on_child: Option<crate::operation::ExecutionScope>,
) -> (ExecuteSession, PlasmHostState, Arc<Mutex<Vec<String>>>) {
    fixture_with_controls(parents, cancel_on_child, None)
}
pub(super) fn fixture_with_controls(
    parents: usize,
    cancel_on_child: Option<crate::operation::ExecutionScope>,
    pause_on_child: Option<Arc<(tokio::sync::Notify, tokio::sync::Notify)>>,
) -> (ExecuteSession, PlasmHostState, Arc<Mutex<Vec<String>>>) {
    let mut cgs = plasm_core::load_schema(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/python_dag_slice"),
    )
    .unwrap();
    cgs.bind_registry_entry_id("fixture");
    let cgs = Arc::new(cgs);
    let contexts = indexmap::IndexMap::from([(
        "fixture".into(),
        Arc::new(CgsContext::entry("fixture", cgs.clone())),
    )]);
    let teaching = TeachingExposureSession::new(&cgs, "fixture", &["Item", "Tag"]);
    let es = ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        contexts,
        "fixture".into(),
        String::new(),
        String::new(),
        None,
        vec!["Item".into(), "Tag".into()],
        Some(teaching),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://127.0.0.1:9".into()),
            ..Default::default()
        },
        Arc::new(Transport {
            calls: calls.clone(),
            parents,
            pause_on_child,
            cancel_on_child,
        }),
        None,
    );
    let host = crate::http::build_plasm_host_state(crate::http::PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(CgsRegistry::from_pairs(vec![(
            "fixture".into(),
            "Fixture".into(),
            vec![],
            cgs,
        )])),
        catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    });
    (es, host, calls)
}
pub(super) fn id(s: &str) -> StepId {
    StepId::new(s).unwrap()
}
pub(super) fn owner(entity: &str) -> PlanQualifiedEntityKey {
    PlanQualifiedEntityKey {
        entry_id: "fixture".into(),
        entity: entity.into(),
    }
}
pub(super) fn query(entity: &str, expr: plasm_core::Expr) -> PlasmStepPayload {
    let mut payload = invoke_step_payload(
        SurfaceKind::Query,
        EffectClass::Read,
        ResultShape::List,
        "query",
    );
    let PlasmStepPayload::Invoke(p) = &mut payload else {
        unreachable!()
    };
    p.qualified_entity = Some(owner(entity));
    p.ir = Some(PlanExprIr {
        expr,
        projection: None,
        display_expr: None,
    });
    payload
}
pub(super) fn program(es: &ExecuteSession) -> (PlasmCompBundle, CorrelatedBody) {
    let mut root = empty_comp(None);
    root.steps.insert(
        "items".into(),
        query(
            "Item",
            plasm_core::Expr::Query(
                plasm_core::QueryExpr::all("Item").with_capability("item_query"),
            ),
        ),
    );
    root.bind.topo = vec![id("items")];
    root.return_ = PlasmReturn::Step { step: id("items") };
    let root =
        PlasmCompBundle::new(crate::plasm_comp_wire::plasm_comp_artifact_from_comp(root).unwrap())
            .unwrap();
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let tag = symbols.entity_sym_for("fixture", "Tag");
    let source=format!("@compute\ndef labels(tags: list[Value[{tag}]]) -> str:\n    return \"|\".join(tag.label for tag in tags)\n");
    let mut body = empty_comp(None);
    let expr = plasm_core::Expr::Query(
        plasm_core::QueryExpr::filtered(
            "Tag",
            plasm_core::Predicate::eq(
                "item_id",
                plasm_core::Value::PlasmInputRef(plasm_core::PlasmInputRef::node_output(
                    "parent",
                    vec!["id".into()],
                )),
            ),
        )
        .with_capability("tag_query"),
    );
    let mut child = query("Tag", expr.clone());
    let PlasmStepPayload::Invoke(p) = &mut child else {
        unreachable!()
    };
    p.ir = None;
    p.ir_template = Some(PlanExprTemplate {
        expr,
        projection: None,
        display_expr: None,
        input_bindings: vec![PlanInputBinding {
            from: "parent".into(),
            to: "parent".into(),
        }],
    });
    body.steps.insert("children".into(), child);
    body.steps.insert(
        "reduced".into(),
        PlasmStepPayload::Map(MapPayload {
            compute: ComputeTemplate {
                source: "children".into(),
                op: ComputeOp::Python {
                    source,
                    entry_id: "fixture".into(),
                    entity: "Tag".into(),
                    catalog_hash: es.cgs.catalog_cgs_hash_hex(),
                    contract_version: 3,
                    input_schema: None,
                    per_row: false,
                },
                schema: SyntheticResultSchema {
                    entity: None,
                    fields: vec![SyntheticFieldSchema {
                        value_type: None,
                        name: OutputName::new("content").unwrap(),
                        value_kind: SyntheticValueKind::String,
                        source: None,
                    }],
                },
                page_size: None,
                collection_alias: None,
            },
            effect_class: EffectClass::ArtifactRead,
            result_shape: ResultShape::Single,
        }),
    );
    body.steps.insert(
        "output".into(),
        PlasmStepPayload::Derive(DerivePayload {
            derive: DeriveTemplate {
                kind: DeriveKind::Map,
                source: Some("parent".into()),
                item_binding: Some(BindingName::new("item").unwrap()),
                inputs: vec![PlanDataInput {
                    node: "reduced".into(),
                    alias: "labels".into(),
                    cardinality: InputCardinality::Singleton,
                }],
                value: PlasmDataValue::Object {
                    fields: BTreeMap::from([
                        (
                            "title".into(),
                            PlasmDataValue::BindingSymbol {
                                binding: "item".into(),
                                path: vec!["title".into()],
                            },
                        ),
                        (
                            "labels".into(),
                            PlasmDataValue::NodeSymbol {
                                node: "reduced".into(),
                                alias: "labels".into(),
                                path: vec!["content".into()],
                            },
                        ),
                    ]),
                },
            },
            effect_class: EffectClass::ArtifactRead,
            result_shape: ResultShape::Single,
        }),
    );
    body.bind.topo = vec![id("children"), id("reduced"), id("output")];
    body.bind.deps = BTreeMap::from([
        (id("children"), BTreeSet::from([id("parent")])),
        (id("reduced"), BTreeSet::from([id("children")])),
        (id("output"), BTreeSet::from([id("parent"), id("reduced")])),
    ]);
    body.bind.holes.insert(
        id("children"),
        vec![PlasmHoleUse {
            step: id("parent"),
            alias: "parent".into(),
        }],
    );
    body.return_ = PlasmReturn::Step { step: id("output") };
    (
        root,
        CorrelatedBody {
            parent: ParentCapture {
                source: id("items"),
                local: id("parent"),
                entity: owner("Item"),
            },
            max_parents: NonZeroU32::new(256).unwrap(),
            body,
        },
    )
}
pub(super) fn on_runtime(f: impl std::future::Future<Output = ()> + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(f)
        })
        .unwrap()
        .join()
        .unwrap();
}

pub(super) fn compose(
    root: PlasmCompBundle,
    body: CorrelatedBody,
) -> Result<PlasmCompBundle, String> {
    let mut comp = root.artifact().comp.clone();
    comp.bind
        .deps
        .insert(id("result"), BTreeSet::from([body.parent.source.clone()]));
    comp.steps
        .insert("result".into(), PlasmStepPayload::MapBody(Box::new(body)));
    comp.bind.topo.push(id("result"));
    comp.return_ = PlasmReturn::Step { step: id("result") };
    PlasmCompBundle::new(crate::plasm_comp_wire::plasm_comp_artifact_from_comp(comp)?)
}
pub(super) async fn execute(
    es: &ExecuteSession,
    host: &PlasmHostState,
    bundle: &PlasmCompBundle,
) -> Result<crate::plasm_plan_run::PlasmPlanRunResult, String> {
    let dry = evaluate_plasm_comp_dry(es, bundle).map_err(|e| e.to_string())?;
    Box::pin(run_plasm_comp(
        es,
        host,
        &es.prompt_hash,
        "map-body",
        bundle,
        true,
        None,
        None,
        Some(dry),
        None,
    ))
    .await
}
pub(super) fn rows(run: &crate::plasm_plan_run::PlasmPlanRunResult) -> Vec<Value> {
    run.return_steps[0]
        .result
        .entities
        .iter()
        .map(|e| {
            let row = plasm_runtime::entity_to_agent_row_json(e, None);
            json!({"title":row["title"],"labels":row["labels"]})
        })
        .collect()
}
