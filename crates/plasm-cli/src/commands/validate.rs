use crate::commands::common;
use indexmap::IndexMap;
use plasm_compile::CmlRequest;
use plasm_core::{
    CapabilityKind, CreateExpr, DeleteExpr, Expr, FieldType, GetExpr, InputFieldSchema,
    InputFieldWire, InputType, InvokeExpr, Predicate, QueryExpr, QueryPagination, Value, CGS,
};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode, SessionMaterialization,
    StreamConsumeOpts,
};
use std::path::Path;

/// Result of validating a single capability.
#[derive(Debug)]
pub enum CheckResult {
    /// CML compiled, HTTP request made, response decoded — fully verified
    Pass(String),
    /// CML compiled, HTTP request made, response reached server but decode had issues
    Warn {
        check: String,
        note: String,
    },
    /// CML is broken — this mapping cannot produce a valid HTTP request
    Fail {
        check: String,
        error: String,
    },
    Skip(String),
}

impl CheckResult {
    fn is_fail(&self) -> bool {
        matches!(self, CheckResult::Fail { .. })
    }
    fn is_warn(&self) -> bool {
        matches!(self, CheckResult::Warn { .. })
    }
}

/// Hermit list mocks cap each page at a small max (`build_with_bounds(..., max_items)`).
/// Requesting more than one page worth exercises the runtime pagination loop.
const VALIDATION_PAGINATION_MAX_ITEMS: usize = 12;

/// Run exhaustive validation of a CGS against a hermit mock.
pub async fn execute(schema: &str, spec: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("Loading schema: {}", schema);
    let cgs = common::load_cgs(Path::new(schema))?;
    println!(
        "  {} entities, {} capabilities",
        cgs.entities.len(),
        cgs.capabilities.len()
    );

    println!("\nStarting hermit mock from: {spec}");
    let spec_path = Path::new(spec);
    if !spec_path.exists() {
        return Err(format!("Spec file not found: {spec}").into());
    }

    let (base_url, _server) = start_hermit(spec_path).await?;
    println!("  Mock serving at {}", base_url);

    let config = ExecutionConfig {
        base_url: Some(base_url.clone()),
        ..ExecutionConfig::default()
    };
    let engine = ExecutionEngine::new(config)?;
    let mut mat = SessionMaterialization::new();

    let mut all_results: Vec<(String, Vec<CheckResult>)> = Vec::new();
    let mut total_pass = 0usize;
    let mut total_warn = 0usize;
    let mut total_fail = 0usize;
    let mut total_skip = 0usize;

    // ── Per-entity checks ────────────────────────────────────────────────────
    for (entity_name, entity) in &cgs.entities {
        let mut results: Vec<CheckResult> = Vec::new();

        // 1. Get by ID — every entity with a Get capability
        if cgs
            .find_capability(entity_name.as_str(), CapabilityKind::Get)
            .is_some()
        {
            results.push(
                check_execution(
                    &format!("get {} by ID", entity_name),
                    Expr::Get(GetExpr::new(entity_name, "test-1")),
                    &cgs,
                    &engine,
                    &mut mat,
                    StreamConsumeOpts::default(),
                )
                .await,
            );
        }

        // 2. Query — every entity with a Query capability
        if let Some(cap) = cgs.find_capability(entity_name.as_str(), CapabilityKind::Query) {
            // 2a. Query with required params (should succeed)
            let required_pred = build_required_predicate(cap, entity, &cgs);
            let query_expr = match required_pred {
                Some(p) => QueryExpr::filtered(entity_name, p),
                None => QueryExpr::all(entity_name),
            };
            results.push(
                check_execution(
                    &format!("query {} (required params)", entity_name),
                    Expr::Query(query_expr.clone()),
                    &cgs,
                    &engine,
                    &mut mat,
                    StreamConsumeOpts::default(),
                )
                .await,
            );

            // 2a′. Paginated query — when mappings declare `pagination`, fetch enough items to
            // cross at least one page boundary (hermit caps pages at a few items).
            if query_mapping_has_pagination(cap) {
                let paginated = query_expr.with_pagination(QueryPagination::default());
                results.push(
                    check_execution(
                        &format!("query {} (paginated collection)", entity_name),
                        Expr::Query(paginated),
                        &cgs,
                        &engine,
                        &mut mat,
                        StreamConsumeOpts {
                            fetch_all: false,
                            max_items: Some(VALIDATION_PAGINATION_MAX_ITEMS),
                            one_page: false,
                            graph_backed_result: false,
                            ..Default::default()
                        },
                    )
                    .await,
                );
            }

            // 2b. Query without required params (should fail at type-check, not CML)
            if cap.input_schema.as_ref().is_some_and(has_required_fields) {
                let bare_result = engine
                    .execute(
                        &Expr::Query(QueryExpr::all(entity_name)),
                        &cgs,
                        &mut SessionMaterialization::new(),
                        Some(ExecutionMode::Live),
                        StreamConsumeOpts::default(),
                        ExecuteOptions::default(),
                    )
                    .await;
                results.push(match bare_result {
                    Err(e) if format!("{e}").contains("VariableNotFound") => CheckResult::Pass(
                        format!("query {entity_name} without required params → CML rejects"),
                    ),
                    Err(_) => CheckResult::Pass(format!(
                        "query {entity_name} without required params → rejected"
                    )),
                    Ok(r) if r.count == 0 => CheckResult::Pass(format!(
                        "query {entity_name} without required params → empty (ok)"
                    )),
                    Ok(_) => CheckResult::Skip(format!(
                        "query {entity_name} without required params → mock returned data anyway"
                    )),
                });
            }
        }

        // 3. Create
        if let Some(cap) = cgs.find_capability(entity_name.as_str(), CapabilityKind::Create) {
            let input = build_fake_input(cap, &cgs);
            results.push(
                check_execution(
                    &format!("create {entity_name}"),
                    Expr::Create(CreateExpr::new(&cap.name, entity_name, input)),
                    &cgs,
                    &engine,
                    &mut mat,
                    StreamConsumeOpts::default(),
                )
                .await,
            );
        }

        // 4. Delete
        if let Some(cap) = cgs.find_capability(entity_name.as_str(), CapabilityKind::Delete) {
            results.push(
                check_execution(
                    &format!("delete {entity_name}"),
                    Expr::Delete(DeleteExpr::new(&cap.name, entity_name, "test-1")),
                    &cgs,
                    &engine,
                    &mut mat,
                    StreamConsumeOpts::default(),
                )
                .await,
            );
        }

        // 5. Update / Action capabilities
        for (cap_name, cap) in &cgs.capabilities {
            if cap.domain.as_str() != entity_name.as_str() {
                continue;
            }
            if !matches!(cap.kind, CapabilityKind::Update | CapabilityKind::Action) {
                continue;
            }
            let input = build_fake_input(cap, &cgs);
            results.push(
                check_execution(
                    &format!(
                        "{} ({})",
                        cap_name,
                        format!("{:?}", cap.kind).to_lowercase()
                    ),
                    Expr::Invoke(InvokeExpr::new(
                        cap_name,
                        entity_name,
                        "test-1",
                        Some(input),
                    )),
                    &cgs,
                    &engine,
                    &mut mat,
                    StreamConsumeOpts::default(),
                )
                .await,
            );
        }

        // 6. Relation traversal — every relation from this entity
        for (rel_name, rel) in &entity.relations {
            if cgs.get_entity(rel.target_resource.as_str()).is_some() {
                let label = format!(
                    "{}.{} → {} traversal",
                    entity_name, rel_name, rel.target_resource
                );
                let relation_expr =
                    plasm_core::relation_validation_expr(&cgs, entity_name, rel_name, rel);

                if let Some(rel_expr) = relation_expr {
                    match rel_expr {
                        Expr::Query(mut rel_query) => {
                            let target_q = rel_query
                                .capability_name
                                .as_ref()
                                .and_then(|name| cgs.capabilities.get(name.as_str()))
                                .or_else(|| {
                                    cgs.find_capability(
                                        rel.target_resource.as_str(),
                                        CapabilityKind::Query,
                                    )
                                });
                            let paginated = target_q.is_some_and(query_mapping_has_pagination);
                            if paginated {
                                rel_query = rel_query.with_pagination(QueryPagination::default());
                            }
                            let consume = if paginated {
                                StreamConsumeOpts {
                                    fetch_all: false,
                                    max_items: Some(VALIDATION_PAGINATION_MAX_ITEMS),
                                    one_page: false,
                                    graph_backed_result: false,
                                    ..Default::default()
                                }
                            } else {
                                StreamConsumeOpts::default()
                            };
                            results.push(
                                check_execution(
                                    &label,
                                    Expr::Query(rel_query),
                                    &cgs,
                                    &engine,
                                    &mut mat,
                                    consume,
                                )
                                .await,
                            );
                        }
                        other => {
                            results.push(
                                check_execution(
                                    &label,
                                    other,
                                    &cgs,
                                    &engine,
                                    &mut mat,
                                    StreamConsumeOpts::default(),
                                )
                                .await,
                            );
                        }
                    }
                } else {
                    results.push(CheckResult::Skip(format!(
                        "{} (requires parent-context materialization not executable in standalone validate)",
                        label
                    )));
                }
            }
        }

        for r in &results {
            match r {
                CheckResult::Pass(_) => total_pass += 1,
                CheckResult::Warn { .. } => total_warn += 1,
                CheckResult::Fail { .. } => total_fail += 1,
                CheckResult::Skip(_) => total_skip += 1,
            }
        }

        all_results.push((entity_name.to_string(), results));
    }

    // ── Print results ────────────────────────────────────────────────────────
    println!();
    for (entity_name, results) in &all_results {
        let has_fail = results.iter().any(|r| r.is_fail());
        let has_warn = results.iter().any(|r| r.is_warn());
        let status = if has_fail {
            "✗"
        } else if has_warn {
            "⚠"
        } else {
            "✓"
        };
        println!("{} {}", status, entity_name);

        for result in results {
            match result {
                CheckResult::Pass(msg) => println!("    ✓ {}", msg),
                CheckResult::Warn { check, note } => {
                    println!("    ⚠ {}", check);
                    println!("      → {}", note);
                }
                CheckResult::Fail { check, error } => {
                    println!("    ✗ {}", check);
                    println!("      → {}", error);
                }
                CheckResult::Skip(msg) => println!("    ~ {}", msg),
            }
        }
    }

    println!("\n─────────────────────────────────────");
    println!(
        "  ✓ Pass: {}  ⚠ Warn: {}  ✗ Fail: {}  ~ Skip: {}",
        total_pass, total_warn, total_fail, total_skip
    );

    if total_warn > 0 {
        println!("\nWarnings indicate the HTTP mapping compiled and reached the server,");
        println!("but the response shape didn't decode as expected. Check:");
        println!("  - Does the API response use a wrapper envelope (e.g. {{\"tasks\": [...]}})?");
        println!("  - Are relation targets correctly defined in domain.yaml?");
        println!("  - Is the capability kind correct (query vs action)?");
    }

    if total_fail > 0 {
        println!("\n{} check(s) FAILED — CML mapping is broken.", total_fail);
        println!("The capability cannot produce a valid HTTP request. Fix mappings.yaml.");
        return Err(format!("{} failures", total_fail).into());
    } else if total_warn == 0 {
        println!("\nAll checks passed.");
    }

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

async fn check_execution(
    label: &str,
    expr: Expr,
    cgs: &CGS,
    engine: &ExecutionEngine,
    mat: &mut SessionMaterialization,
    consume: StreamConsumeOpts,
) -> CheckResult {
    match engine
        .execute(
            &expr,
            cgs,
            mat,
            Some(ExecutionMode::Live),
            consume,
            ExecuteOptions::default(),
        )
        .await
    {
        Ok(result) => {
            if result.count == 0 && !matches!(expr, Expr::Delete(_)) {
                // Request succeeded but returned no entities — could be mock returning
                // empty/wrong shape, or the capability is action-typed but returns nothing
                CheckResult::Warn {
                    check: label.to_string(),
                    note: "Request succeeded but no entities decoded (mock may return unexpected shape)".into(),
                }
            } else {
                CheckResult::Pass(label.to_string())
            }
        }
        Err(e) => {
            let msg = format!("{e}");
            categorize_error(label, &msg)
        }
    }
}

fn categorize_error(label: &str, msg: &str) -> CheckResult {
    // ── Hard failures: CML is structurally broken ─────────────────────────
    if msg.contains("VariableNotFound") {
        return CheckResult::Fail {
            check: label.to_string(),
            error: format!(
                "CML variable not in env — check path/query var names in mappings.yaml: {}",
                extract_var(msg)
            ),
        };
    }
    if msg.contains("ConfigurationError") {
        return CheckResult::Fail {
            check: label.to_string(),
            error: format!(
                "CML template is invalid JSON — check mappings.yaml syntax: {}",
                trim_error(msg)
            ),
        };
    }
    if msg.contains("CmlError") {
        return CheckResult::Fail {
            check: label.to_string(),
            error: format!("CML compilation failed: {}", trim_error(msg)),
        };
    }

    // ── Soft warnings: request reached server, but decode/response issue ──
    if msg.contains("DecodeError") || msg.contains("TypeMismatch") {
        return CheckResult::Warn {
            check: label.to_string(),
            note: format!(
                "Response shape mismatch — API may wrap response in an envelope: {}",
                trim_error(msg)
            ),
        };
    }
    if msg.contains("PathNotFound") {
        return CheckResult::Warn {
            check: label.to_string(),
            note: format!(
                "Decoder path not found in response — check entity field names in domain.yaml: {}",
                trim_error(msg)
            ),
        };
    }
    if msg.contains("status: 404") || msg.contains("404") {
        return CheckResult::Warn {
            check: label.to_string(),
            note: "Mock returned 404 — check path template in mappings.yaml matches spec".into(),
        };
    }
    if msg.contains("status: 4") {
        return CheckResult::Warn {
            check: label.to_string(),
            note: format!(
                "Mock returned 4xx — check required params and body structure: {}",
                trim_error(msg)
            ),
        };
    }
    if msg.contains("status: 5") {
        return CheckResult::Warn {
            check: label.to_string(),
            note: format!(
                "Mock returned 5xx — hermit internal error, likely unsupported spec pattern: {}",
                trim_error(msg)
            ),
        };
    }
    if msg.contains("RequestError") || msg.contains("Connection") || msg.contains("connect") {
        return CheckResult::Fail {
            check: label.to_string(),
            error: format!(
                "Cannot reach mock server — is hermit running? {}",
                trim_error(msg)
            ),
        };
    }

    // ── Unknown: surface the full error for diagnosis ─────────────────────
    CheckResult::Warn {
        check: label.to_string(),
        note: format!("Unexpected error (investigate): {}", trim_error(msg)),
    }
}

fn trim_error(msg: &str) -> String {
    // Keep the first 120 chars of the error message
    let s = msg.trim();
    if s.len() > 120 {
        format!("{}...", &s[..120])
    } else {
        s.to_string()
    }
}

fn extract_var(msg: &str) -> String {
    // Extract the variable name from "VariableNotFound { name: \"foo\" }"
    if let Some(start) = msg.find("name: \"") {
        let rest = &msg[start + 7..];
        if let Some(end) = rest.find('"') {
            return format!("\"{}\"", &rest[..end]);
        }
    }
    trim_error(msg)
}

fn fake_value_for_input_field(f: &InputFieldSchema, cgs: &CGS) -> Option<Value> {
    match &f.wire {
        InputFieldWire::Registry(_) => {
            let nv = f.named_value(cgs).ok()?;
            Some(fake_value_for_type(
                &nv.field_type,
                nv.allowed_values.as_deref(),
            ))
        }
        InputFieldWire::Inline(ty) => Some(fake_value_for_input_type(ty.as_ref(), cgs)),
    }
}

fn fake_value_for_input_type(ty: &InputType, cgs: &CGS) -> Value {
    match ty {
        InputType::None => Value::Null,
        InputType::Value {
            field_type,
            allowed_values,
        } => fake_value_for_type(field_type, allowed_values.as_deref()),
        InputType::Object { fields, .. } => {
            let mut m = IndexMap::new();
            for field in fields.iter().filter(|x| x.required) {
                if let Some(v) = fake_value_for_input_field(field, cgs) {
                    m.insert(field.name.clone(), v);
                }
            }
            Value::Object(m)
        }
        InputType::Array { element_type, .. } => {
            Value::Array(vec![fake_value_for_input_type(element_type.as_ref(), cgs)])
        }
        InputType::Union { variants } => {
            let Some(v) = variants.first() else {
                return Value::Null;
            };
            let mut m = IndexMap::new();
            m.insert(v.wire.field.clone(), Value::String(v.wire.value.clone()));
            for field in v.fields.iter().filter(|x| x.required) {
                if let Some(val) = fake_value_for_input_field(field, cgs) {
                    m.insert(field.name.clone(), val);
                }
            }
            Value::Object(m)
        }
    }
}

fn build_required_predicate(
    cap: &plasm_core::CapabilitySchema,
    _entity: &plasm_core::EntityDef,
    cgs: &plasm_core::CGS,
) -> Option<Predicate> {
    let input_schema = cap.input_schema.as_ref()?;
    let InputType::Object { fields, .. } = &input_schema.input_type else {
        return None;
    };

    let comparisons: Vec<Predicate> = fields
        .iter()
        .filter(|f| f.required)
        .filter_map(|f| {
            let val = fake_value_for_input_field(f, cgs)?;
            Some(Predicate::eq(f.name.clone(), val))
        })
        .collect();

    match comparisons.len() {
        0 => None,
        1 => Some(comparisons.into_iter().next().unwrap()),
        _ => Some(Predicate::and(comparisons)),
    }
}

fn build_fake_input(cap: &plasm_core::CapabilitySchema, cgs: &plasm_core::CGS) -> Value {
    let Some(input_schema) = &cap.input_schema else {
        return Value::Null;
    };
    let InputType::Object { fields, .. } = &input_schema.input_type else {
        return Value::Null;
    };

    let mut obj = IndexMap::new();
    for f in fields.iter().filter(|f| f.required) {
        if let Some(v) = fake_value_for_input_field(f, cgs) {
            obj.insert(f.name.clone(), v);
        }
    }
    if obj.is_empty() {
        Value::Null
    } else {
        Value::Object(obj)
    }
}

fn fake_value_for_type(ft: &FieldType, allowed: Option<&[String]>) -> Value {
    if let Some(vals) = allowed {
        if let Some(first) = vals.first() {
            return Value::String(first.clone());
        }
    }
    match ft {
        FieldType::Integer => Value::Integer(1),
        FieldType::Number => Value::Float(1.0),
        FieldType::Boolean => Value::Bool(false),
        FieldType::EntityRef { .. } => Value::String("1".into()),
        _ => Value::String("plasm-test".into()),
    }
}

fn has_required_fields(schema: &plasm_core::InputSchema) -> bool {
    if let InputType::Object { fields, .. } = &schema.input_type {
        return fields.iter().any(|f| f.required);
    }
    false
}

/// True when the query capability's CML template declares a `pagination` block.
fn query_mapping_has_pagination(cap: &plasm_core::CapabilitySchema) -> bool {
    serde_json::from_value::<CmlRequest>(cap.mapping.template.0.clone())
        .ok()
        .is_some_and(|r| r.pagination.is_some())
}

async fn start_hermit(
    spec_path: &Path,
) -> Result<(String, tokio::task::JoinHandle<()>), Box<dyn std::error::Error>> {
    let spec = beavuck_hermit::spec_loader::load(spec_path);
    let routes = beavuck_hermit::spec_parser::extract_routes(&spec);
    let router = beavuck_hermit::router::build_with_bounds(routes, 1, 5);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    // Determine API base path from spec servers
    let base_path = {
        let content = std::fs::read_to_string(spec_path)?;
        let json: serde_json::Value = if spec_path.extension().is_some_and(|e| e == "json") {
            serde_json::from_str(&content)?
        } else {
            let y: serde_yaml::Value = serde_yaml::from_str(&content)?;
            serde_json::to_value(y)?
        };
        json.get("servers")
            .and_then(|s| s.as_array())
            .and_then(|a| a.first())
            .and_then(|s| s.get("url"))
            .and_then(|u| u.as_str())
            .and_then(|url| {
                if let Ok(parsed) = url::Url::parse(url) {
                    let path = parsed.path().to_string();
                    if path.len() > 1 {
                        Some(path)
                    } else {
                        None
                    }
                } else if url.starts_with('/') {
                    Some(url.to_string())
                } else {
                    None
                }
            })
            .unwrap_or_default()
    };

    let base_url = format!("http://127.0.0.1:{port}{base_path}");

    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    Ok((base_url, server))
}
