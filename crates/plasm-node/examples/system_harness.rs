//! Throwaway system comparison bridge. No production registration or API changes.
//! JSON-line controller owns provider calls, pinned recall receipts and a stateful abstract HTTP backend.
use async_trait::async_trait;
use plasm_agent_core::{
    discovery_matcher::{
        online::{select_retrieved, SearchContext},
        MatchChoice, RELEVANCE_MODEL,
    },
    discovery_store::DiscoveryAuthorization,
    intent_provenance::IntentProvenance,
};
use plasm_core::prerequisites::{prerequisite_closure, DeploymentBindings};
use plasm_node::AgentEngine;
use plasm_runtime::{auth::ResolvedAuth, error::RuntimeError, HttpTransport};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::Path,
    sync::{Arc, Mutex},
};

type Error = Box<dyn std::error::Error + Send + Sync>;
fn receive() -> Result<Value, Error> {
    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        return Err("controller closed".into());
    }
    Ok(serde_json::from_str(&line)?)
}
fn emit(value: Value) -> Result<(), Error> {
    println!("{value}");
    io::stdout().flush()?;
    Ok(())
}
struct Bridge(Mutex<()>);
#[async_trait]
impl HttpTransport for Bridge {
    async fn send_compiled_http(
        &self,
        base: &str,
        request: &plasm_compile::CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(Value, Option<String>), RuntimeError> {
        let result = (|| -> Result<_, Error> {
            let _guard = self.0.lock().map_err(|_| "bridge lock poisoned")?;
            emit(
                json!({"event":"http", "url":plasm_runtime::http_transport::compiled_http_url(base, request), "method":format!("{:?}",request.method), "body":request.body}),
            )?;
            let response = receive()?;
            plasm_runtime::execution::record_transport_response_received();
            if let Some(error) = response.get("error") {
                return Err(error.to_string().into());
            }
            Ok((response["body"].clone(), None))
        })();
        result.map_err(|source| RuntimeError::HostTransport { source })
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(Value, Option<String>), RuntimeError> {
        Err(RuntimeError::HostTransport {
            source: "unexpected pagination in frozen fixtures".into(),
        })
    }
}
#[tokio::main]
async fn main() -> Result<(), Error> {
    let init = receive()?;
    let mut catalogs = BTreeMap::new();
    let mut compiled = BTreeMap::new();
    for (id, path) in init["catalogs"].as_object().ok_or("catalogs required")? {
        let mut cgs =
            plasm_core::loader::load_schema_dir(Path::new(path.as_str().ok_or("path required")?))?;
        cgs.bind_registry_entry_id(id);
        compiled.insert(
            id.clone(),
            Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs)?),
        );
        catalogs.insert(id.clone(), cgs);
    }
    let deployments: DeploymentBindings = serde_json::from_value(
        init.get("bindings")
            .cloned()
            .unwrap_or(json!({"bindings":[]})),
    )?;
    let allowed = DiscoveryAuthorization::catalogs(catalogs.keys().cloned().collect());
    let arm = init["arm"].as_str().ok_or("arm required")?;
    let mut engine = AgentEngine::from_generation(
        catalogs.clone(),
        compiled,
        "abstract-system-ablation".into(),
        std::sync::Arc::new(Default::default()),
    );
    let mut visits = BTreeMap::new();
    let mut history = Vec::<String>::new();
    let transport: Arc<dyn HttpTransport> = Arc::new(Bridge(Mutex::new(())));
    emit(json!({"event":"ready"}))?;
    loop {
        let command = receive()?;
        let op = command["op"].as_str().unwrap_or("");
        let result: Result<Value,Error> = async {
            match op {
                "pack_fixture" => {
                    let dir=Path::new(command["directory"].as_str().ok_or("directory required")?);
                    std::fs::create_dir_all(dir)?;
                    let mut manifests=Vec::new();
                    for (id,cgs) in &catalogs {
                        let cgs_bytes=plasm_core::catalog_il::cgs_to_catalog_il_bytes(cgs)?;
                        let recipes=plasm_compile::compile_cgs_capability_templates(cgs)?;
                        let recipes_bytes=serde_json::to_vec(&recipes)?;
                        // Model-free fixture vectors, never used by this lexical selection arm.
                        let capabilities=plasm_core::catalog_discovery::capability_documents(cgs)?.into_iter().map(|document| {
                            let mut embedding=vec![0.0;1536];embedding[0]=1.0;
                            plasm_core::catalog_discovery::EmbeddedCapability {document,embedding}
                        }).collect();
                        let discovery=plasm_core::catalog_discovery::CatalogDiscoveryArtifact {
                            entry_id:id.clone(),cgs_hash:cgs.catalog_cgs_hash_hex(),renderer_version:plasm_core::catalog_discovery::DISCOVERY_RENDERER_VERSION,
                            profile:Default::default(),capabilities,prerequisites:cgs.prerequisites.clone()
                        };
                        discovery.validate(cgs)?;
                        let discovery_bytes=serde_json::to_vec(&discovery)?;
                        let manifest=plasm_core::catalog_il::CatalogManifest {
                            format_version:3,entry_id:id.clone(),version:cgs.version,cgs_hash:cgs.catalog_cgs_hash_hex(),label:id.clone(),tags:vec![],
                            cgs_json:format!("{id}.cgs.json"),recipes_json:format!("{id}.recipes.json"),recipes_hash:plasm_core::catalog_discovery::content_hash(&recipes_bytes),
                            discovery_json:format!("{id}.discovery.json"),discovery_hash:plasm_core::catalog_discovery::content_hash(&discovery_bytes),embedding_profile:Default::default()
                        };
                        for (name,bytes) in [(&manifest.cgs_json,cgs_bytes),(&manifest.recipes_json,recipes_bytes),(&manifest.discovery_json,discovery_bytes)] { std::fs::write(dir.join(name),bytes)?; }
                        let name=format!("{id}.manifest.json");std::fs::write(dir.join(&name),serde_json::to_vec(&manifest)?)?;manifests.push(name);
                    }
                    std::fs::write(dir.join("catalog-set.json"),serde_json::to_vec(&json!({"format_version":3,"manifests":manifests}))?)?;
                    std::fs::write(dir.join("deployment-bindings.json"),serde_json::to_vec(&deployments)?)?;
                    Ok(json!({"packed":manifests}))
                }
                "load_packed" => Ok(serde_json::to_value(engine.load_catalog(Path::new(command["path"].as_str().ok_or("path required")?))?)?),
                "activate_fixture" => {
                    let requested:DeploymentBindings=serde_json::from_str(command["bindings"].as_str().ok_or("bindings required")?)?;
                    if serde_json::to_value(requested)? != serde_json::to_value(&deployments)? { return Err("fixture deployment bindings changed".into()); }
                    Ok(json!({"activated":true}))
                }
                "discover" => {
                    let intent=command["intent"].as_str().ok_or("intent required")?;
                    history.push(intent.to_owned());
                    let provenance=IntentProvenance::from_turns(history.clone())?;
                    let existing=engine.exposed_capabilities();
                    let mut selected=Vec::new(); let mut audit=json!({"diagnostic_ceiling":true});
                    if arm=="ceiling" {
                        selected=serde_json::from_value(init["ceiling"].clone())?;
                    } else {
                        // Diagnostic controllers supply explicit pinned retrieval; this bridge
                        // never reintroduces retired test-only catalogue enumeration.
                        let generation = init["generation"].as_str().ok_or("non-ceiling discovery requires pinned generation in initialization")?;
                        let recall = serde_json::from_value(command.get("recall").cloned().ok_or("discover requires pinned recall receipt")?)?;
                        let operations = plasm_agent_core::discovery_matcher::operations::OperationSet::from_excerpts(intent, &[intent.to_owned()])?;
                        let (retrieval,matches,receipt)=select_retrieved(SearchContext { model:RELEVANCE_MODEL,intent:&provenance,generation,catalogs:&catalogs,deployments:&deployments,allowed:&allowed,exposed:&existing,visits:&visits }, &recall, operations, |request| async move {
                            let body:Value=serde_json::from_str(request.body())?;
                            if arm=="deterministic" {
                                let answers:serde_json::Map<String,Value>=body["questions"].as_object().ok_or("missing questions")?.keys().map(|id|(id.clone(),json!({"type":"choice","choice":"relevant","confidence":1.0,"probabilities":{"relevant":1.0,"unrelated":0.0,"uncertain":0.0}}))).collect();
                                emit(json!({"event":"selection_packet","body":body,"provider_called":false}))?;
                                Ok(json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","answers":answers}).to_string())
                            } else {
                                emit(json!({"event":"decision","body":body}))?;
                                let answer=receive()?;
                                if answer.get("error").is_some() {return Err(answer.to_string().into());}
                                Ok(answer["raw"].as_str().ok_or("raw answer required")?.to_owned())
                            }
                        }).await?;
                        for candidate in &retrieval.candidates {
                            let answer=matches.matches.iter().find(|m|m.capability_id==candidate.id).ok_or("missing match")?;
                            *visits.entry(candidate.reference.clone()).or_insert(0)+=1;
                            if answer.choice!=MatchChoice::Unrelated {selected.push(candidate.reference.clone());}
                        }
                        audit=json!({"retrieval":retrieval,"matches":matches,"receipt":receipt});
                    }
                    let refs=catalogs.iter().map(|(k,v)|(k.clone(),v)).collect();
                    let closure=prerequisite_closure(&refs,&deployments,&selected,&allowed.catalogs)?;
                    let teaching=engine.expose_routing(intent,&closure).await?;
                    emit(json!({"event":"discovery_audit","audit":audit,"closure":closure,"exposed":engine.exposed_capabilities()}))?;
                    Ok(serde_json::to_value(teaching)?)
                }
                "introspect" => Ok(serde_json::to_value(engine.introspect_catalog(command["catalog"].as_str().ok_or("catalog required")?)?)?),
                "review" => Ok(serde_json::to_value(engine.dry_run(command["program"].as_str().ok_or("program required")?).await?)?),
                "run" => Ok(serde_json::to_value(engine.run_plan_live(command["run_ref"].as_str().ok_or("run_ref required")?,transport.clone()).await?)?),
                "quit" => Ok(json!({"closed":true})),
                _ => Err("unknown bridge command".into()),
            }
        }.await;
        match result {
            Ok(value) => emit(json!({"event":"result","value":value}))?,
            Err(error) => emit(json!({"event":"result","error":error.to_string()}))?,
        }
        if op == "quit" {
            break;
        }
    }
    Ok(())
}
