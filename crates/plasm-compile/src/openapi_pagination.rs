//! Catalog compilation against the external OpenAPI pagination contract.
use crate::{parse_capability_template, template_pagination, CmlError};
use plasm_core::{CapabilityKind, CGS};
use serde_json::Value;

fn paths(expr: &Value) -> Vec<String> {
    match expr.get("type").and_then(Value::as_str) {
        Some("literal") | Some("const") => expr
            .get("value")
            .and_then(Value::as_str)
            .map(|s| vec![s.to_owned()])
            .unwrap_or_default(),
        Some("var") => vec!["*".into()],
        Some("if") => paths(&expr["then_expr"])
            .into_iter()
            .chain(paths(&expr["else_expr"]))
            .collect(),
        _ => Vec::new(),
    }
}
fn matches_path(template: &Value, path: &str) -> bool {
    let Some(parts) = template.get("path").and_then(Value::as_array) else {
        return false;
    };
    let mut candidates = vec![String::new()];
    for part in parts {
        let choices = paths(part);
        candidates = candidates
            .iter()
            .flat_map(|prefix| choices.iter().map(move |part| format!("{prefix}/{part}")))
            .collect();
    }
    candidates.iter().any(|candidate| {
        let left: Vec<_> = candidate.trim_matches('/').split('/').collect();
        let right: Vec<_> = path.trim_matches('/').split('/').collect();
        left.len() == right.len() && left.iter().zip(right).all(|(a, b)| *a == "*" || *a == b)
    })
}
fn resolved<'a>(root: &'a Value, value: &'a Value) -> &'a Value {
    value
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix('#'))
        .and_then(|r| root.pointer(r))
        .unwrap_or(value)
}
/// Structural operation inventory against the external specification. This is a
/// necessary completeness check, not proof that inputs, output fields or branch
/// predicates are semantically correct; Hermit execution validates those separately.
/// Unknown path expressions are not credited as coverage.
pub fn uncovered_openapi_operations(cgs: &CGS, spec: &Value) -> Vec<String> {
    let mut mapped = Vec::new();
    for cap in cgs.capabilities.values() {
        let Some(mapping) = &cap.mapping else {
            continue;
        };
        let raw = &mapping.template.0;
        let Some(method) = raw["method"].as_str() else {
            continue;
        };
        let Some(parts) = raw["path"].as_array() else {
            continue;
        };
        let choices = |name: &str| -> Vec<String> {
            cap.query_surface_fields()
                .chain(cap.invocation_object_fields())
                .find(|f| f.name == name)
                .and_then(|f| f.named_value(cgs).ok())
                .and_then(|nv| nv.allowed_values.clone())
                .unwrap_or_else(|| vec!["*".into()])
        };
        let mut candidates = vec![String::new()];
        for part in parts {
            let options = coverage_path_choices(part, &choices);
            candidates = candidates
                .iter()
                .flat_map(|prefix| {
                    options
                        .iter()
                        .map(move |suffix| format!("{prefix}/{suffix}"))
                })
                .collect();
        }
        mapped.extend(
            candidates
                .into_iter()
                .map(|path| (method.to_ascii_lowercase(), path)),
        );
    }
    let mut missing = Vec::new();
    if let Some(routes) = spec["paths"].as_object() {
        for (path, item) in routes {
            for method in [
                "get", "post", "put", "patch", "delete", "head", "options", "trace",
            ] {
                let Some(operation) = item.get(method) else {
                    continue;
                };
                let mut routes = vec![path.clone()];
                for parameter in item["parameters"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .chain(operation["parameters"].as_array().into_iter().flatten())
                {
                    let parameter = resolved(spec, parameter);
                    if parameter["in"] != "path" {
                        continue;
                    }
                    if let (Some(name), Some(values)) = (
                        parameter["name"].as_str(),
                        parameter["schema"]["enum"].as_array(),
                    ) {
                        routes = routes
                            .iter()
                            .flat_map(|route| {
                                values
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(move |value| route.replace(&format!("{{{name}}}"), value))
                            })
                            .collect();
                    }
                }
                if routes.iter().any(|route| {
                    !mapped.iter().any(|(m, candidate)| {
                        m == method
                            && (coverage_path_matches(candidate, route)
                                || coverage_path_matches(candidate, path))
                    })
                }) {
                    missing.push(format!("{} {path}", method.to_ascii_uppercase()));
                }
            }
        }
    }
    missing.sort();
    missing
}

fn coverage_path_choices(expr: &Value, values: &impl Fn(&str) -> Vec<String>) -> Vec<String> {
    match expr["type"].as_str() {
        Some("literal" | "const") => expr["value"]
            .as_str()
            .map(|s| vec![s.into()])
            .unwrap_or_default(),
        Some("var") => expr["name"].as_str().map(values).unwrap_or_default(),
        Some("if") => coverage_path_choices(&expr["then_expr"], values)
            .into_iter()
            .chain(coverage_path_choices(&expr["else_expr"], values))
            .collect(),
        Some("format") => {
            let Some(template) = expr["template"].as_str() else {
                return Vec::new();
            };
            let mut paths = vec![template.to_owned()];
            if let Some(vars) = expr["vars"].as_object() {
                for (name, value) in vars {
                    let choices = coverage_path_choices(value, values);
                    paths = paths
                        .iter()
                        .flat_map(|path| {
                            choices
                                .iter()
                                .map(move |v| path.replace(&format!("{{{name}}}"), v))
                        })
                        .collect();
                }
            }
            paths.into_iter().filter(|p| !p.contains('{')).collect()
        }
        _ => Vec::new(),
    }
}

fn coverage_path_matches(candidate: &str, route: &str) -> bool {
    let left: Vec<_> = candidate.trim_matches('/').split('/').collect();
    let right: Vec<_> = route.trim_matches('/').split('/').collect();
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| *a == b || (*a == "*" && b.starts_with('{') && b.ends_with('}')))
}

/// Reject paginated list operations without a CML driver, including pagination
/// omitted from both CGS inputs and CML. Only explicit paging parameter pairs are
/// recognized; a lone `limit` is not sufficient evidence of pagination.
pub fn validate_cgs_openapi_pagination(cgs: &CGS, spec: &Value) -> Result<(), CmlError> {
    let Some(routes) = spec.get("paths").and_then(Value::as_object) else {
        return Ok(());
    };
    for (name, cap) in &cgs.capabilities {
        if !matches!(cap.kind, CapabilityKind::Query | CapabilityKind::Search) {
            continue;
        }
        let Some(mapping) = &cap.mapping else {
            continue;
        };
        let raw = &mapping.template.0;
        if raw
            .get("method")
            .and_then(Value::as_str)
            .is_none_or(|m| !m.eq_ignore_ascii_case("GET"))
        {
            continue;
        }
        let parsed = parse_capability_template(raw)?;
        for (path, item) in routes {
            if !matches_path(raw, path) {
                continue;
            }
            let Some(op) = item.get("get") else {
                continue;
            };
            let params: Vec<_> = item
                .get("parameters")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .chain(
                    op.get("parameters")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten(),
                )
                .map(|p| resolved(spec, p))
                .filter(|p| p["in"] == "query")
                .filter_map(|p| p["name"].as_str())
                .collect();
            let paged = [
                ("page_index", "page_limit"),
                ("offset", "limit"),
                ("page", "per_page"),
                ("page", "page_size"),
                ("startAt", "maxResults"),
            ]
            .iter()
            .any(|(a, b)| params.contains(a) && params.contains(b));
            if paged && template_pagination(&parsed).is_none() {
                return Err(CmlError::InvalidTemplate { message: format!("capability `{name}`: OpenAPI GET {path} declares pagination but CML omits `pagination:`") });
            }
        }
    }
    Ok(())
}
/// Validate an OpenAPI JSON companion when compiling a catalog directory.
pub fn validate_catalog_openapi_pagination(
    cgs: &CGS,
    directory: &std::path::Path,
) -> Result<(), CmlError> {
    let path = directory.join("openapi.json");
    if !path.exists() {
        return Ok(());
    }
    let bytes = std::fs::read(&path).map_err(|e| CmlError::InvalidTemplate {
        message: format!("read {}: {e}", path.display()),
    })?;
    let spec = serde_json::from_slice(&bytes).map_err(|e| CmlError::InvalidTemplate {
        message: format!("parse {}: {e}", path.display()),
    })?;
    validate_cgs_openapi_pagination(cgs, &spec)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operation_coverage_does_not_confuse_identity_with_static_route() {
        assert!(coverage_path_matches("/items/*", "/items/{item_id}"));
        assert!(!coverage_path_matches("/items/*", "/items/count"));
        assert!(!coverage_path_matches("/items/fixed", "/items/{item_id}"));
        let expr = serde_json::json!({"type":"format","template":"group/{id}/items","vars":{"id":{"type":"var","name":"id"}}});
        assert_eq!(
            coverage_path_choices(&expr, &|_| vec!["*".into()]),
            vec!["group/*/items"]
        );
    }

    #[test]
    fn operation_coverage_reports_missing_methods_and_enum_routes() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_pagination_matrix");
        let mut cgs = plasm_core::load_schema_dir(&root).unwrap();
        let mut cap = cgs
            .capabilities
            .values()
            .find(|c| c.kind == CapabilityKind::Query)
            .unwrap()
            .clone();
        cap.mapping.as_mut().unwrap().template.0 = serde_json::json!({"method":"GET","path":[{"type":"literal","value":"items"},{"type":"var","name":"id"}]});
        cgs.capabilities.clear();
        cgs.capabilities.insert(cap.name.clone(), cap);
        let spec = serde_json::json!({"paths":{"/items/{item_id}":{"get":{},"post":{}},"/items/count":{"get":{}}}});
        assert_eq!(
            uncovered_openapi_operations(&cgs, &spec),
            vec!["GET /items/count", "POST /items/{item_id}"]
        );
        assert_eq!(
            coverage_path_choices(
                &serde_json::json!({"type":"var","name":"shelf"}),
                &|_| vec!["inbox".into(), "outbox".into()]
            ),
            vec!["inbox", "outbox"]
        );
    }

    #[test]
    fn conditional_paths_cover_every_branch() {
        let t = serde_json::json!({"path":[{"type":"literal","value":"api"},{"type":"if","then_expr":{"type":"const","value":"library/items"},"else_expr":{"type":"const","value":"items"}}]});
        assert!(matches_path(&t, "/api/library/items"));
        assert!(matches_path(&t, "/api/items"));
        assert!(!matches_path(&t, "/api/other"));
    }
    #[test]
    fn external_pagination_omitted_from_domain_and_mapping_is_rejected() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_pagination_matrix");
        let mut cgs = plasm_core::load_schema_dir(&root).unwrap();
        let cap = cgs
            .capabilities
            .values_mut()
            .find(|c| c.kind == CapabilityKind::Query)
            .unwrap();
        let raw = &mut cap.mapping.as_mut().unwrap().template.0;
        let driver = raw.as_object_mut().unwrap().remove("pagination").unwrap();
        raw["method"] = Value::String("GET".into());
        raw["path"] = serde_json::json!([{"type":"literal","value":"items"}]);
        let spec = serde_json::json!({"paths":{"/items":{"get":{"parameters":[{"in":"query","name":"page_index"},{"in":"query","name":"page_limit"}]}}}});
        assert!(validate_cgs_openapi_pagination(&cgs, &spec)
            .unwrap_err()
            .to_string()
            .contains("omits"));
        let cap = cgs
            .capabilities
            .values_mut()
            .find(|c| c.kind == CapabilityKind::Query)
            .unwrap();
        cap.mapping.as_mut().unwrap().template.0["pagination"] = driver;
        validate_cgs_openapi_pagination(&cgs, &spec).unwrap();
    }
}
