use crate::commands::common;
use indexmap::IndexMap;
use plasm_compile::CmlRequest;
use plasm_core::{
    CapabilityKind, CreateExpr, DeleteExpr, Expr, FieldType, GetExpr, InputFieldSchema,
    InputFieldWire, InputType, InvokeExpr, MoneyWireFormat, Predicate, QueryExpr, QueryPagination,
    Value, ValueWireFormat, CGS,
};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode, SessionMaterialization,
    StreamConsumeOpts,
};
use std::path::Path;

/// Result of validating a single capability.
#[derive(Debug)]
pub enum CheckResult {
    /// Execution succeeded and declared provides were checked for decoded rows.
    Pass(String),
    /// Execution returned no decoded entities; conformance remains unverified.
    Warn {
        check: String,
        note: String,
    },
    /// Execution or declared response conformance failed.
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
pub async fn execute(
    schema: &str,
    spec: &str,
    require_complete: bool,
) -> Result<(), Box<dyn std::error::Error>> {
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

    let external = beavuck_hermit::spec_loader::load(spec_path);
    let external = serde_json::to_value(external)?;
    let missing = plasm_compile::uncovered_openapi_operations(&cgs, &external);
    if !missing.is_empty() {
        let message = format!(
            "{} OpenAPI operations have no catalog mapping:\n{}",
            missing.len(),
            missing.join("\n")
        );
        if require_complete {
            return Err(message.into());
        }
        println!("{message}");
    }
    plasm_compile::validate_cgs_openapi_pagination(&cgs, &external)?;
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

        let identity = entity
            .fields
            .get(entity.id_field.as_str())
            .and_then(|field| field.named_value(&cgs).ok())
            .map(fake_value_for_named_value)
            .unwrap_or_else(|| Value::String("test-1".into()));
        let identity = match identity {
            Value::String(value) => value,
            Value::Integer(value) => value.to_string(),
            value => serde_json::to_string(&plasm_core::plasm_value_to_json(&value))?,
        };

        // Exercise every collection operation, including secondary queries and searches.
        for cap in cgs.capabilities.values().filter(|cap| {
            cap.domain == *entity_name
                && matches!(cap.kind, CapabilityKind::Query | CapabilityKind::Search)
        }) {
            // 2a. Query with required params (should succeed)
            let required_pred = build_required_predicate(cap, entity, &cgs);
            let mut query_expr = match required_pred {
                Some(p) => QueryExpr::filtered(entity_name, p),
                None => QueryExpr::all(entity_name),
            };
            query_expr.capability_name = Some(cap.name.clone());
            results.push(
                check_execution(
                    &format!("{} (required params)", cap.name),
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
                        &format!("{} (paginated collection)", cap.name),
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
            if cap.has_any_required_param() {
                let mut bare = QueryExpr::all(entity_name);
                bare.capability_name = Some(cap.name.clone());
                let bare_result = engine
                    .execute(
                        &Expr::Query(bare),
                        &cgs,
                        &mut SessionMaterialization::new(),
                        Some(ExecutionMode::Live),
                        StreamConsumeOpts::default(),
                        ExecuteOptions::for_catalog(&cgs).expect("validated catalog must compile"),
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

        // 1. Get by ID — every entity with a Get capability
        if let Some(cap) = cgs.find_capability(entity_name.as_str(), CapabilityKind::Get) {
            let get = GetExpr::from_ref(
                mat.get_entities_by_type(entity_name.as_str())
                    .first()
                    .map(|row| row.reference.clone())
                    .unwrap_or_else(|| plasm_core::Ref::new(entity_name, identity.clone())),
            )
            .with_capability(cap.name.clone());
            if let Value::Object(params) = build_fake_input(cap, &cgs) {
                mat.stamp_capability_params(&get.reference, params);
            }
            results.push(
                check_execution(
                    &format!("get {entity_name} by ID"),
                    Expr::Get(get),
                    &cgs,
                    &engine,
                    &mut mat,
                    StreamConsumeOpts::default(),
                )
                .await,
            );
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
            let mut delete = DeleteExpr::new(&cap.name, entity_name, identity.clone());
            delete.input = Some(build_fake_input(cap, &cgs).into());
            results.push(
                check_execution(
                    &format!("delete {entity_name}"),
                    Expr::Delete(delete),
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
                        if cap.requires_receiver() {
                            identity.as_str()
                        } else {
                            ""
                        },
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
                            if let Some(cap) = target_q {
                                rel_query = build_relation_probe_query(rel_query, cap, &cgs);
                            }
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

    if total_fail > 0 || total_warn > 0 {
        return Err(format!(
            "Hermit conformance incomplete: {total_fail} failures, {total_warn} warnings"
        )
        .into());
    }
    println!("\nAll exercised checks passed; {total_skip} skipped checks remain unverified.");

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
            ExecuteOptions::for_catalog(cgs).expect("validated catalog must compile"),
        )
        .await
    {
        Ok(result) => {
            let capability = match &expr {
                Expr::Create(create) => cgs.capabilities.get(create.capability.as_str()),
                Expr::Invoke(invoke) => cgs.capabilities.get(invoke.capability.as_str()),
                Expr::Get(get) => get
                    .capability_name
                    .as_ref()
                    .and_then(|name| cgs.capabilities.get(name.as_str()))
                    .or_else(|| {
                        cgs.find_capability(get.reference.entity_type.as_str(), CapabilityKind::Get)
                    }),
                Expr::Query(query) => query
                    .capability_name
                    .as_ref()
                    .and_then(|name| cgs.capabilities.get(name.as_str()))
                    .or_else(|| cgs.find_capability(query.entity.as_str(), CapabilityKind::Query)),
                _ => None,
            };
            if let Some(capability) = capability {
                let missing: Vec<_> = cgs
                    .effective_provides(capability)
                    .into_iter()
                    .filter(|field| {
                        result
                            .entities
                            .iter()
                            .any(|row| !row.fields.contains_key(field.as_str()))
                    })
                    .collect();
                if !missing.is_empty() {
                    return CheckResult::Fail {
                        check: label.to_owned(),
                        error: format!(
                            "Declared provides absent from OpenAPI response: {missing:?}"
                        ),
                    };
                }
            }
            let side_effect = capability.is_some_and(|cap| {
                cap.output_schema.as_ref().is_some_and(|output| {
                    matches!(
                        output.output_type,
                        plasm_core::OutputType::SideEffect { .. }
                    )
                })
            });
            if result.count == 0 && !matches!(expr, Expr::Delete(_)) && !side_effect {
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
    CheckResult::Fail {
        check: label.to_owned(),
        error: trim_error(msg),
    }
}

fn trim_error(msg: &str) -> String {
    // Keep the first 120 chars of the error message
    let s = msg.trim();
    if s.len() > 120 {
        format!("{}...", s.chars().take(120).collect::<String>())
    } else {
        s.to_string()
    }
}

fn fake_value_for_named_value(nv: &plasm_core::NamedValueSchema) -> Value {
    match nv.domain.profile {
        Some(plasm_core::value_domain::ProfileId::Email) => {
            Value::String("contract@example.com".into())
        }
        Some(plasm_core::value_domain::ProfileId::E164) => Value::String("+12025550123".into()),
        _ => fake_value_for_type(
            &nv.field_type,
            nv.allowed_values.as_deref(),
            nv.value_format.as_ref(),
        ),
    }
}

fn fake_value_for_input_field(f: &InputFieldSchema, cgs: &CGS) -> Option<Value> {
    match &f.wire {
        InputFieldWire::Registry(_) => {
            let nv = f.named_value(cgs).ok()?;
            Some(fake_value_for_named_value(nv))
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
        } => fake_value_for_type(field_type, allowed_values.as_deref(), None),
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
    let comparisons: Vec<Predicate> = cap
        .query_surface_fields()
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

fn build_relation_probe_query(
    mut query: QueryExpr,
    cap: &plasm_core::CapabilitySchema,
    cgs: &CGS,
) -> QueryExpr {
    let pivots = query
        .predicate
        .as_ref()
        .map(Predicate::referenced_fields)
        .unwrap_or_default();
    let mut predicates: Vec<_> = cap
        .query_surface_fields()
        .filter(|field| field.required || pivots.contains(&field.name))
        .filter_map(|field| {
            fake_value_for_input_field(field, cgs)
                .map(|value| Predicate::eq(field.name.clone(), value))
        })
        .collect();
    query.predicate = match predicates.len() {
        0 => None,
        1 => predicates.pop(),
        _ => Some(Predicate::and(predicates)),
    };
    query
}

#[cfg(test)]
mod relation_probe_tests {
    use super::*;

    #[test]
    fn validation_probe_uses_e164_value() {
        let cgs = plasm_core::loader::load_schema_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/validation_probe_params"),
        )
        .unwrap();
        let field = cgs
            .get_capability("probe_query")
            .unwrap()
            .query_surface_fields()
            .find(|field| field.name == "phone_number")
            .unwrap();
        assert_eq!(
            fake_value_for_input_field(field, &cgs),
            Some(Value::String("+12025550123".into()))
        );
    }

    #[test]
    fn relation_probe_binds_required_auth_and_typed_scope() {
        let cgs = plasm_core::loader::load_schema_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/validation_probe_params"),
        )
        .unwrap();
        let cap = cgs.get_capability("probe_query").unwrap();
        let query = QueryExpr::filtered("ProbeRecord", Predicate::eq("owner_email", "1"))
            .with_capability("probe_query");
        let query = build_relation_probe_query(query, cap, &cgs);
        assert_eq!(
            query.predicate,
            Some(Predicate::and(vec![
                Predicate::eq("owner_email", "contract@example.com"),
                Predicate::eq("access_token", "plasm-test"),
            ]))
        );
    }
}

fn build_fake_input(cap: &plasm_core::CapabilitySchema, cgs: &plasm_core::CGS) -> Value {
    let mut obj = IndexMap::new();
    for f in cap.invocation_object_fields().filter(|f| f.required) {
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

fn fake_value_for_type(
    ft: &FieldType,
    allowed: Option<&[String]>,
    value_format: Option<&ValueWireFormat>,
) -> Value {
    if let Some(vals) = allowed {
        if let Some(first) = vals.first() {
            return Value::String(first.clone());
        }
    }
    match ft {
        FieldType::DigitId => Value::String("1234".into()),
        FieldType::Integer => Value::Integer(1),
        FieldType::Number => Value::Float(1.0),
        FieldType::Money => {
            let fmt = match value_format {
                Some(ValueWireFormat::Money(m)) => *m,
                _ => MoneyWireFormat::decimal_string(),
            };
            plasm_core::money::normalize(Value::String("1".into()), fmt, None)
                .unwrap_or_else(|_| Value::String("1".into()))
        }
        FieldType::Date => {
            let format = match value_format {
                Some(ValueWireFormat::Temporal(format)) => *format,
                _ => plasm_core::TemporalWireFormat::Rfc3339,
            };
            plasm_core::temporal::normalize_temporal_value(
                Value::String("2026-01-02T03:04:05Z".into()),
                format,
            )
            .expect("fixed temporal probe must be encodable")
        }
        FieldType::Boolean => Value::Bool(false),
        FieldType::EntityRef { .. } => Value::String("1".into()),
        _ => Value::String("plasm-test".into()),
    }
}

/// True when the query capability's CML template declares a `pagination` block.
fn query_mapping_has_pagination(cap: &plasm_core::CapabilitySchema) -> bool {
    let Ok(mapping) = cap.require_mapping() else {
        return false;
    };
    serde_json::from_value::<CmlRequest>(mapping.template.0.clone())
        .ok()
        .is_some_and(|r| r.pagination.is_some())
}

async fn start_hermit(
    spec_path: &Path,
) -> Result<(String, tokio::task::JoinHandle<()>), Box<dyn std::error::Error>> {
    let spec = beavuck_hermit::spec_loader::load(spec_path);
    let routes = beavuck_hermit::spec_parser::extract_routes(&spec);
    let router = beavuck_hermit::router::build_spec_responses(routes);

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

#[cfg(test)]
mod temporal_probe_tests {
    use super::*;
    #[test]
    fn temporal_probe_respects_declared_wire_type() {
        for format in [
            plasm_core::TemporalWireFormat::Rfc3339,
            plasm_core::TemporalWireFormat::Iso8601Date,
            plasm_core::TemporalWireFormat::UnixSec,
            plasm_core::TemporalWireFormat::UnixMs,
        ] {
            let value = fake_value_for_type(
                &FieldType::Date,
                None,
                Some(&ValueWireFormat::Temporal(format)),
            );
            assert_eq!(
                plasm_core::temporal::normalize_temporal_value(value.clone(), format).unwrap(),
                value
            );
        }
    }
}
