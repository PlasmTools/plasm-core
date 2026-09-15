use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use yaml_serde::Value;

use crate::http_method::HttpMethod;
use rayon::prelude::*;

pub fn resolve_ref<'a>(root: &'a Value, ref_str: &str) -> Option<&'a Value> {
    let path = ref_str.trim_start_matches('#').trim_start_matches('/');
    let mut current = root;
    for key in path.split('/') {
        current = current.get(key)?;
    }
    Some(current)
}

pub fn flatten_schema(schema: &Value, root: &Value) -> Value {
    let mut visiting = HashSet::new();
    flatten_inner(schema, root, None, &mut visiting)
}

pub fn flatten_schema_forced(schema: &Value, root: &Value, variant: &str) -> Value {
    let mut visiting = HashSet::new();
    flatten_inner(schema, root, Some(variant), &mut visiting)
}

enum RefStep {
    Followed(Value),
    Stop,
    NotARef,
}

fn step_ref(current: &Value, root: &Value, visiting: &mut HashSet<String>) -> RefStep {
    let Some(ref_str) = current
        .get("$ref")
        .and_then(|v| v.as_str())
        .map(String::from)
    else {
        return RefStep::NotARef;
    };
    if visiting.contains(&ref_str) {
        return RefStep::Stop;
    }
    visiting.insert(ref_str.clone());
    match resolve_ref(root, &ref_str) {
        Some(resolved) => RefStep::Followed(resolved.clone()),
        None => RefStep::Stop,
    }
}

fn flatten_inner(
    schema: &Value,
    root: &Value,
    forced: Option<&str>,
    visiting: &mut HashSet<String>,
) -> Value {
    let mut current = schema.clone();

    loop {
        match step_ref(&current, root, visiting) {
            RefStep::Followed(v) => {
                current = v;
                continue;
            }
            RefStep::Stop => break,
            RefStep::NotARef => {}
        }

        if current.get("allOf").is_some() {
            let merged = flatten_all_of(&current, root, forced, visiting);
            let mut result = yaml_serde::Mapping::new();
            result.insert(Value::String("type".into()), Value::String("object".into()));
            result.insert(Value::String("properties".into()), Value::Mapping(merged));
            return Value::Mapping(result);
        }

        if let Some(variant) = pick_composite_variant(&current, root, forced) {
            current = variant;
            continue;
        }

        break;
    }

    current
}

fn flatten_all_of(
    schema: &Value,
    root: &Value,
    forced: Option<&str>,
    visiting: &mut HashSet<String>,
) -> yaml_serde::Mapping {
    let mut merged = yaml_serde::Mapping::new();
    let Some(items) = schema.get("allOf").and_then(|v| v.as_sequence()) else {
        return merged;
    };
    let mut queue: VecDeque<Value> = items.iter().cloned().collect();
    while let Some(item) = queue.pop_front() {
        if let Some(props) = collect_properties_from_item(item, root, forced, visiting, &mut queue)
        {
            for (k, v) in props {
                merged.insert(k, v);
            }
        }
    }
    merged
}

fn collect_properties_from_item(
    item: Value,
    root: &Value,
    forced: Option<&str>,
    visiting: &mut HashSet<String>,
    queue: &mut VecDeque<Value>,
) -> Option<yaml_serde::Mapping> {
    let mut current = item;
    loop {
        match step_ref(&current, root, visiting) {
            RefStep::Followed(v) => {
                current = v;
                continue;
            }
            RefStep::Stop => return None,
            RefStep::NotARef => {}
        }
        if let Some(inner_items) = current.get("allOf").and_then(|v| v.as_sequence()) {
            queue.extend(inner_items.iter().cloned());
            return None;
        }
        if let Some(variant) = pick_composite_variant(&current, root, forced) {
            current = variant;
            continue;
        }
        return current
            .get("properties")
            .and_then(|v| v.as_mapping())
            .cloned();
    }
}

fn pick_composite_variant(schema: &Value, root: &Value, forced: Option<&str>) -> Option<Value> {
    let variants = ["oneOf", "anyOf"].iter().find_map(|key| {
        let seq = schema.get(*key)?.as_sequence()?;
        if seq.is_empty() { None } else { Some(seq) }
    })?;

    if let Some(forced_key) = forced
        && let Some(variant) = try_pick_forced(schema, root, forced_key)
    {
        return Some(variant);
    }

    Some(variants[0].clone())
}

fn try_pick_forced(schema: &Value, root: &Value, forced_key: &str) -> Option<Value> {
    let mapping = schema.get("discriminator")?.get("mapping")?.as_mapping()?;
    let lookup = Value::String(forced_key.to_string());
    let ref_str = mapping.get(&lookup)?.as_str()?;
    resolve_ref(root, ref_str).cloned()
}

pub fn find_discriminator(schema: &Value, root: &Value) -> Option<(String, Vec<String>)> {
    let mut visiting = HashSet::new();
    let mut queue: VecDeque<Value> = VecDeque::from([schema.clone()]);
    while let Some(item) = queue.pop_front() {
        if let Some(info) = search_item_for_discriminator(item, root, &mut visiting, &mut queue) {
            return Some(info);
        }
    }
    None
}

fn search_item_for_discriminator(
    item: Value,
    root: &Value,
    visiting: &mut HashSet<String>,
    queue: &mut VecDeque<Value>,
) -> Option<(String, Vec<String>)> {
    let mut current = item;
    loop {
        match step_ref(&current, root, visiting) {
            RefStep::Followed(v) => {
                current = v;
                continue;
            }
            RefStep::Stop => return None,
            RefStep::NotARef => {}
        }
        for composite_key in &["oneOf", "anyOf"] {
            if current.get(*composite_key).is_some()
                && let Some(info) = discriminator_info(&current)
            {
                return Some(info);
            }
        }
        if let Some(items) = current.get("allOf").and_then(|v| v.as_sequence()) {
            queue.extend(items.iter().cloned());
        }
        return None;
    }
}

fn discriminator_info(schema: &Value) -> Option<(String, Vec<String>)> {
    let disc = schema.get("discriminator")?;
    let prop_name = disc.get("propertyName")?.as_str()?;
    let keys = disc
        .get("mapping")
        .and_then(|m| m.as_mapping())
        .map(|m| {
            m.keys()
                .filter_map(|k| k.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Some((prop_name.to_string(), keys))
}

#[derive(Clone)]
pub struct RouteConfig {
    pub axum_path: String,
    pub method: HttpMethod,
    pub status_code: u16,
    pub body: Option<serde_json::Value>,
    pub discriminator_field: Option<String>,
    pub variants: Option<HashMap<String, serde_json::Value>>,
    pub item_generator: Option<Arc<dyn Fn() -> serde_json::Value + Send + Sync>>,
    pub read_only_fields: HashSet<String>,
    /// Standard offset/limit parameters and count/next/previous/results envelope.
    pub offset_pagination: bool,
}

impl Default for RouteConfig {
    fn default() -> Self {
        Self {
            axum_path: String::new(),
            method: HttpMethod::Get,
            status_code: 200,
            body: None,
            discriminator_field: None,
            variants: None,
            item_generator: None,
            read_only_fields: Default::default(),
            offset_pagination: false,
        }
    }
}

pub fn extract_routes(spec: &Value) -> Vec<RouteConfig> {
    let spec_arc = Arc::new(spec.clone());
    let base = server_base_path(spec);
    let Some(paths) = spec.get("paths").and_then(|v| v.as_mapping()) else {
        return Vec::new();
    };

    let mut routes = Vec::new();
    for (path_val, path_item) in paths {
        let Some(path_str) = path_val.as_str() else {
            continue;
        };
        let axum_path = format!("{}{}", base, path_str);
        for &method in HttpMethod::ALL {
            if let Some(mut route) = path_item
                .get(method.as_str())
                .and_then(|op| route_for_operation(method, op, spec, &spec_arc))
            {
                let parameters = path_item
                    .get(method.as_str())
                    .and_then(|op| op.get("parameters"))
                    .and_then(|p| p.as_sequence());
                let declares = |name: &str| {
                    parameters.is_some_and(|params| {
                        params.iter().any(|p| {
                            p.get("in").and_then(|v| v.as_str()) == Some("query")
                                && p.get("name").and_then(|v| v.as_str()) == Some(name)
                                && p.get("schema")
                                    .and_then(|s| s.get("type"))
                                    .and_then(|v| v.as_str())
                                    == Some("integer")
                        })
                    })
                };
                route.offset_pagination = method == HttpMethod::Get
                    && declares("offset")
                    && declares("limit")
                    && route.body.as_ref().is_some_and(|body| {
                        ["count", "next", "previous", "results"]
                            .iter()
                            .all(|key| body.get(key).is_some())
                    });
                route.axum_path = axum_path.clone();
                routes.push(route);
            }
        }
    }
    routes
}

// `trim_end_matches('/')` never produces `"/"`, so any `trimmed == "/"` check
// is unreachable dead code that no test can exercise.
#[cfg_attr(test, mutants::skip)]
fn server_base_path(spec: &Value) -> String {
    let url = spec
        .get("servers")
        .and_then(|s| s.get(0))
        .and_then(|s| s.get("url"))
        .and_then(|u| u.as_str())
        .unwrap_or("");
    let path = if url.contains("://") {
        url.splitn(4, '/')
            .nth(3)
            .map(|p| format!("/{}", p))
            .unwrap_or_default()
    } else {
        url.to_string()
    };
    let trimmed = path.trim_end_matches('/');
    if trimmed == "/" || trimmed.is_empty() {
        String::new()
    } else {
        trimmed.to_string()
    }
}

fn route_for_operation(
    method: HttpMethod,
    operation: &Value,
    spec: &Value,
    spec_arc: &Arc<Value>,
) -> Option<RouteConfig> {
    let responses = operation.get("responses")?;
    let (status_code, response) = first_success_response(responses);

    let schema = extract_response_schema(response, spec);

    Some(match schema {
        Some(s) => route_from_schema(method, status_code, s, spec, spec_arc),
        None => RouteConfig {
            method,
            status_code,
            ..Default::default()
        },
    })
}

fn route_from_schema(
    method: HttpMethod,
    status_code: u16,
    schema: &Value,
    spec: &Value,
    spec_arc: &Arc<Value>,
) -> RouteConfig {
    let read_only_fields = collect_read_only_fields(schema, spec);
    if method.uses_request_body()
        && let Some((disc_field, keys)) = find_discriminator(schema, spec)
    {
        let use_examples = crate::resource_generator::use_examples();
        let variants = keys
            .par_iter()
            .map(|key| {
                crate::resource_generator::set_use_examples(use_examples);
                (
                    key.clone(),
                    crate::resource_generator::generate(schema, spec, Some(key)),
                )
            })
            .collect();
        return RouteConfig {
            method,
            status_code,
            discriminator_field: Some(disc_field),
            variants: Some(variants),
            read_only_fields,
            ..Default::default()
        };
    }
    let item_generator = build_item_generator(schema, spec, spec_arc);
    RouteConfig {
        method,
        status_code,
        body: Some(crate::resource_generator::generate(schema, spec, None)),
        item_generator,
        read_only_fields,
        ..Default::default()
    }
}

fn collect_read_only_fields(schema: &Value, root: &Value) -> HashSet<String> {
    let flat = flatten_schema(schema, root);
    let mut fields = HashSet::new();
    if let Some(props) = flat.get("properties").and_then(|v| v.as_mapping()) {
        for (k, v) in props {
            if let Some(key) = k.as_str()
                && v.get("readOnly").and_then(|b| b.as_bool()).unwrap_or(false)
            {
                fields.insert(key.to_string());
            }
        }
    }
    fields
}

fn build_item_generator(
    response_schema: &Value,
    root: &Value,
    spec_arc: &Arc<Value>,
) -> Option<Arc<dyn Fn() -> serde_json::Value + Send + Sync>> {
    let item_schema = extract_item_schema(response_schema, root)?;
    let spec_for_gen = Arc::clone(spec_arc);
    Some(Arc::new(move || {
        crate::resource_generator::generate(&item_schema, &spec_for_gen, None)
    }))
}

fn extract_item_schema(response_schema: &Value, root: &Value) -> Option<Value> {
    let flat = flatten_schema(response_schema, root);
    // Direct array response
    if flat.get("type").and_then(|t| t.as_str()) == Some("array") {
        return flat.get("items").cloned();
    }
    // Envelope object with an array property
    flat.get("properties")
        .and_then(|p| p.as_mapping())
        .and_then(|m| {
            m.values()
                .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("array"))
        })
        .and_then(|arr_schema| arr_schema.get("items"))
        .cloned()
}

fn first_success_response(responses: &Value) -> (u16, &Value) {
    if let Some(mapping) = responses.as_mapping() {
        let mut candidates: Vec<(u16, &Value)> = mapping
            .iter()
            .filter_map(|(k, v)| {
                let code: u16 = k.as_str()?.parse().ok()?;
                let success_range = 200..300;
                success_range.contains(&code).then_some((code, v))
            })
            .collect();
        candidates.sort_by_key(|&(code, _)| code);
        if let Some((code, resp)) = candidates.into_iter().next() {
            return (code, resp);
        }
    }
    (200, responses)
}

fn extract_response_schema<'a>(response: &'a Value, root: &'a Value) -> Option<&'a Value> {
    let resp = if let Some(ref_str) = response.get("$ref").and_then(|v| v.as_str()) {
        resolve_ref(root, ref_str)?
    } else {
        response
    };

    resp.get("content")
        .and_then(|c| c.get("application/json"))
        .and_then(|j| j.get("schema"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_method::HttpMethod;
    use crate::resource_generator::{UseExamplesGuard, set_use_examples};
    use crate::spec_loader::load_all;

    fn yaml(s: &str) -> Value {
        yaml_serde::from_str(s).unwrap()
    }

    // --- resolve_ref ---

    #[test]
    fn resolve_ref_finds_nested_value() {
        let root = yaml("a:\n  b:\n    c: found");
        let result = resolve_ref(&root, "#/a/b/c");
        assert_eq!(result.and_then(|v| v.as_str()), Some("found"));
    }

    #[test]
    fn resolve_ref_returns_none_for_missing_key() {
        let root = yaml("a: 1");
        assert!(resolve_ref(&root, "#/missing").is_none());
    }

    #[test]
    fn resolve_ref_returns_none_for_missing_nested_key() {
        let root = yaml("a:\n  b: 1");
        assert!(resolve_ref(&root, "#/a/missing").is_none());
    }

    // --- flatten_schema ---

    #[test]
    fn flatten_schema_returns_plain_schema_unchanged() {
        let root = yaml("{}");
        let schema = yaml("type: string\nexample: hello");
        let flat = flatten_schema(&schema, &root);
        assert_eq!(flat.get("type").and_then(|v| v.as_str()), Some("string"));
        assert_eq!(flat.get("example").and_then(|v| v.as_str()), Some("hello"));
    }

    #[test]
    fn flatten_schema_follows_ref() {
        let root = yaml("Foo:\n  type: string\n  example: hi");
        let schema = yaml("$ref: '#/Foo'");
        let flat = flatten_schema(&schema, &root);
        assert_eq!(flat.get("type").and_then(|v| v.as_str()), Some("string"));
    }

    #[test]
    fn flatten_schema_unresolvable_ref_returns_schema_unchanged() {
        let root = yaml("{}");
        let schema = yaml("$ref: '#/Missing'");
        let flat = flatten_schema(&schema, &root);
        assert_eq!(flat.get("$ref").and_then(|v| v.as_str()), Some("#/Missing"));
    }

    #[test]
    fn flatten_schema_merges_all_of_properties() {
        let root = yaml("{}");
        let schema = yaml(
            "allOf:\n\
             \x20 - type: object\n\
             \x20   properties:\n\
             \x20     a:\n\
             \x20       type: string\n\
             \x20 - type: object\n\
             \x20   properties:\n\
             \x20     b:\n\
             \x20       type: integer",
        );
        let flat = flatten_schema(&schema, &root);
        assert!(flat.get("properties").and_then(|p| p.get("a")).is_some());
        assert!(flat.get("properties").and_then(|p| p.get("b")).is_some());
    }

    #[test]
    fn flatten_schema_all_of_later_entry_wins_on_collision() {
        let root = yaml("{}");
        let schema = yaml(
            "allOf:\n\
             \x20 - type: object\n\
             \x20   properties:\n\
             \x20     x:\n\
             \x20       example: first\n\
             \x20 - type: object\n\
             \x20   properties:\n\
             \x20     x:\n\
             \x20       example: second",
        );
        let flat = flatten_schema(&schema, &root);
        let example = flat
            .get("properties")
            .and_then(|p| p.get("x"))
            .and_then(|x| x.get("example"))
            .and_then(|e| e.as_str());
        assert_eq!(example, Some("second"));
    }

    #[test]
    fn flatten_schema_picks_first_one_of_by_default() {
        let root = yaml("{}");
        let schema = yaml(
            "oneOf:\n\
             \x20 - type: object\n\
             \x20   properties:\n\
             \x20     k:\n\
             \x20       example: first\n\
             \x20 - type: object\n\
             \x20   properties:\n\
             \x20     k:\n\
             \x20       example: second",
        );
        let flat = flatten_schema(&schema, &root);
        let example = get_example(&flat);

        assert_eq!(example, Some("first"));
    }

    #[test]
    fn flatten_schema_picks_first_any_of_by_default() {
        let root = yaml("{}");
        let schema = yaml(
            "anyOf:\n\
             \x20 - type: object\n\
             \x20   properties:\n\
             \x20     k:\n\
             \x20       example: first\n\
             \x20 - type: object\n\
             \x20   properties:\n\
             \x20     k:\n\
             \x20       example: second",
        );
        let flat = flatten_schema(&schema, &root);
        let example = get_example(&flat);

        assert_eq!(example, Some("first"));
    }

    #[test]
    fn flatten_schema_forced_picks_mapped_variant() {
        let root = yaml(
            "A:\n  type: object\n  properties:\n    k:\n      example: a\n\
             B:\n  type: object\n  properties:\n    k:\n      example: b",
        );
        let schema = yaml(
            "oneOf:\n\
             \x20 - $ref: '#/A'\n\
             \x20 - $ref: '#/B'\n\
             discriminator:\n\
             \x20 propertyName: k\n\
             \x20 mapping:\n\
             \x20   a: '#/A'\n\
             \x20   b: '#/B'",
        );
        let flat = flatten_schema_forced(&schema, &root, "b");
        let example = get_example(&flat);

        assert_eq!(example, Some("b"));
    }

    #[test]
    fn flatten_schema_forced_falls_back_to_first_when_key_not_in_mapping() {
        let root = yaml(
            "A:\n  type: object\n  properties:\n    k:\n      example: a\n\
             B:\n  type: object\n  properties:\n    k:\n      example: b",
        );
        let schema = yaml(
            "oneOf:\n\
             \x20 - $ref: '#/A'\n\
             \x20 - $ref: '#/B'\n\
             discriminator:\n\
             \x20 propertyName: k\n\
             \x20 mapping:\n\
             \x20   a: '#/A'\n\
             \x20   b: '#/B'",
        );
        let flat = flatten_schema_forced(&schema, &root, "unknown");
        let example = get_example(&flat);

        assert_eq!(example, Some("a"));
    }

    fn get_example(flat: &Value) -> Option<&str> {
        flat.get("properties")
            .and_then(|p| p.get("k"))
            .and_then(|k| k.get("example"))
            .and_then(|e| e.as_str())
    }
    // --- find_discriminator ---

    #[test]
    fn find_discriminator_finds_direct_one_of_discriminator() {
        let root = yaml("{}");
        let schema = yaml(
            "oneOf:\n\
             \x20 - type: object\n\
             discriminator:\n\
             \x20 propertyName: type\n\
             \x20 mapping:\n\
             \x20   a: '#/a'\n\
             \x20   b: '#/b'",
        );
        let (field, keys) = find_discriminator(&schema, &root).unwrap();
        assert_eq!(field, "type");
        assert!(keys.contains(&"a".to_string()));
        assert!(keys.contains(&"b".to_string()));
    }

    #[test]
    fn find_discriminator_finds_any_of_discriminator() {
        let root = yaml("{}");
        let schema = yaml(
            "anyOf:\n\
             \x20 - type: object\n\
             discriminator:\n\
             \x20 propertyName: kind\n\
             \x20 mapping:\n\
             \x20   x: '#/x'",
        );
        let (field, _) = find_discriminator(&schema, &root).unwrap();
        assert_eq!(field, "kind");
    }

    #[test]
    fn find_discriminator_finds_discriminator_inside_all_of() {
        let root = yaml("{}");
        let schema = yaml(
            "allOf:\n\
             \x20 - oneOf:\n\
             \x20     - type: object\n\
             \x20   discriminator:\n\
             \x20     propertyName: kind\n\
             \x20     mapping:\n\
             \x20       x: '#/x'",
        );
        let (field, _) = find_discriminator(&schema, &root).unwrap();
        assert_eq!(field, "kind");
    }

    #[test]
    fn find_discriminator_follows_ref() {
        let root = yaml(
            "Poly:\n\
             \x20 oneOf:\n\
             \x20   - type: object\n\
             \x20 discriminator:\n\
             \x20   propertyName: type\n\
             \x20   mapping:\n\
             \x20     a: '#/a'",
        );
        let schema = yaml("$ref: '#/Poly'");
        let (field, _) = find_discriminator(&schema, &root).unwrap();
        assert_eq!(field, "type");
    }

    #[test]
    fn find_discriminator_returns_none_when_absent() {
        let root = yaml("{}");
        let schema = yaml("type: object\nproperties:\n  x:\n    type: string");
        assert!(find_discriminator(&schema, &root).is_none());
    }

    // --- extract_routes ---

    #[test]
    fn extract_routes_returns_empty_for_spec_without_paths() {
        let spec = yaml("info:\n  title: Test");
        assert!(extract_routes(&spec).is_empty());
    }

    #[test]
    fn extract_routes_delete_with_204_has_no_body() {
        let spec = yaml(
            "paths:\n\
             \x20 /items/{id}:\n\
             \x20   delete:\n\
             \x20     responses:\n\
             \x20       '204':\n\
             \x20         description: Deleted",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].status_code, 204);
        assert!(routes[0].body.is_none());
        assert!(routes[0].variants.is_none());
    }

    #[test]
    fn extract_routes_get_generates_body_from_schema() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let spec = yaml(
            "paths:\n\
             \x20 /items:\n\
             \x20   get:\n\
             \x20     responses:\n\
             \x20       '200':\n\
             \x20         content:\n\
             \x20           application/json:\n\
             \x20             schema:\n\
             \x20               type: object\n\
             \x20               properties:\n\
             \x20                 name:\n\
             \x20                   type: string\n\
             \x20                   example: hello\n\
             \x20         description: OK",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, HttpMethod::Get);
        assert_eq!(routes[0].status_code, 200);
        assert_eq!(
            routes[0].body.as_ref().unwrap()["name"],
            serde_json::json!("hello")
        );
    }

    #[test]
    fn extract_routes_response_without_schema_has_no_body() {
        let spec = yaml(
            "paths:\n\
             \x20 /items:\n\
             \x20   options:\n\
             \x20     responses:\n\
             \x20       '200':\n\
             \x20         description: OK",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes.len(), 1);
        assert!(routes[0].body.is_none());
    }

    #[test]
    fn extract_routes_post_with_discriminator_generates_variants() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let spec = yaml(
            "components:\n\
             \x20 schemas:\n\
             \x20   A:\n\
             \x20     type: object\n\
             \x20     properties:\n\
             \x20       k: {type: string, example: a}\n\
             \x20   B:\n\
             \x20     type: object\n\
             \x20     properties:\n\
             \x20       k: {type: string, example: b}\n\
             paths:\n\
             \x20 /items:\n\
             \x20   post:\n\
             \x20     responses:\n\
             \x20       '201':\n\
             \x20         content:\n\
             \x20           application/json:\n\
             \x20             schema:\n\
             \x20               oneOf:\n\
             \x20                 - $ref: '#/components/schemas/A'\n\
             \x20                 - $ref: '#/components/schemas/B'\n\
             \x20               discriminator:\n\
             \x20                 propertyName: k\n\
             \x20                 mapping:\n\
             \x20                   a: '#/components/schemas/A'\n\
             \x20                   b: '#/components/schemas/B'\n\
             \x20         description: Created",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].discriminator_field.as_deref(), Some("k"));
        let variants = routes[0].variants.as_ref().unwrap();
        assert_eq!(variants["a"]["k"], serde_json::json!("a"));
        assert_eq!(variants["b"]["k"], serde_json::json!("b"));
        assert!(routes[0].body.is_none());
    }

    #[test]
    fn load_all_returns_routes_from_all_specs_in_parallel() {
        let path = std::path::PathBuf::from("specs_assets/taskflow.openapi.yml");
        let routes = load_all(&[path.clone(), path]);
        assert!(routes.len() >= 2, "expected routes from both specs");
    }

    #[test]
    fn flatten_schema_all_of_item_without_properties_is_skipped() {
        let root = yaml("{}");
        let schema = yaml(
            "allOf:\n\
             \x20 - type: string\n\
             \x20 - type: object\n\
             \x20   properties:\n\
             \x20     x:\n\
             \x20       type: string\n\
             \x20       example: kept",
        );
        let flat = flatten_schema(&schema, &root);
        assert!(flat.get("properties").and_then(|p| p.get("x")).is_some());
    }

    #[test]
    fn extract_routes_ignores_non_string_path_keys() {
        let spec = yaml(
            "paths:\n\
             \x20 123:\n\
             \x20   get:\n\
             \x20     responses:\n\
             \x20       '200':\n\
             \x20         description: OK",
        );
        assert!(extract_routes(&spec).is_empty());
    }

    #[test]
    fn extract_routes_falls_back_to_200_when_no_success_code() {
        let spec = yaml(
            "paths:\n\
             \x20 /items:\n\
             \x20   get:\n\
             \x20     responses:\n\
             \x20       '500':\n\
             \x20         description: Error",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].status_code, 200);
        assert!(routes[0].body.is_none());
    }

    #[test]
    fn extract_routes_resolves_ref_response() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let spec = yaml(
            "components:\n\
             \x20 responses:\n\
             \x20   MyResponse:\n\
             \x20     content:\n\
             \x20       application/json:\n\
             \x20         schema:\n\
             \x20           type: object\n\
             \x20           properties:\n\
             \x20             id:\n\
             \x20               type: string\n\
             \x20               example: abc\n\
             paths:\n\
             \x20 /items:\n\
             \x20   get:\n\
             \x20     responses:\n\
             \x20       '200':\n\
             \x20         $ref: '#/components/responses/MyResponse'",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes.len(), 1);
        assert_eq!(
            routes[0].body.as_ref().unwrap()["id"],
            serde_json::json!("abc")
        );
    }

    // --- server base path ---

    #[test]
    fn extract_routes_prefixes_paths_with_server_base_path() {
        let spec = yaml(
            "servers:\n\
             \x20 - url: /api/dog-cafe\n\
             paths:\n\
             \x20 /dogs:\n\
             \x20   get:\n\
             \x20     responses:\n\
             \x20       '200':\n\
             \x20         description: OK",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes[0].axum_path, "/api/dog-cafe/dogs");
    }

    #[test]
    fn extract_routes_extracts_path_from_full_server_url() {
        let spec = yaml(
            "servers:\n\
             \x20 - url: http://127.0.0.1:8532/api/dog-cafe\n\
             paths:\n\
             \x20 /dogs:\n\
             \x20   get:\n\
             \x20     responses:\n\
             \x20       '200':\n\
             \x20         description: OK",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes[0].axum_path, "/api/dog-cafe/dogs");
    }

    #[test]
    fn extract_routes_with_root_server_url_does_not_prefix() {
        let spec = yaml(
            "servers:\n\
             \x20 - url: /\n\
             paths:\n\
             \x20 /dogs:\n\
             \x20   get:\n\
             \x20     responses:\n\
             \x20       '200':\n\
             \x20         description: OK",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes[0].axum_path, "/dogs");
    }

    #[test]
    fn extract_routes_with_no_servers_does_not_prefix() {
        let spec = yaml(
            "paths:\n\
             \x20 /dogs:\n\
             \x20   get:\n\
             \x20     responses:\n\
             \x20       '200':\n\
             \x20         description: OK",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes[0].axum_path, "/dogs");
    }

    #[test]
    fn extract_routes_preserves_path_parameters_after_prefix() {
        let spec = yaml(
            "servers:\n\
             \x20 - url: /api/v1\n\
             paths:\n\
             \x20 /dogs/{id}:\n\
             \x20   get:\n\
             \x20     responses:\n\
             \x20       '200':\n\
             \x20         description: OK",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes[0].axum_path, "/api/v1/dogs/{id}");
    }

    #[test]
    fn extract_routes_preserves_openapi_path_format() {
        let spec = yaml(
            "paths:\n\
             \x20 /projects/{projectId}:\n\
             \x20   get:\n\
             \x20     responses:\n\
             \x20       '200':\n\
             \x20         description: OK",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes[0].axum_path, "/projects/{projectId}");
    }

    // --- read_only_fields collection ---

    #[test]
    fn extract_routes_post_collects_read_only_fields_from_response_schema() {
        let spec = yaml(
            "paths:\n\
             \x20 /items:\n\
             \x20   post:\n\
             \x20     responses:\n\
             \x20       '201':\n\
             \x20         content:\n\
             \x20           application/json:\n\
             \x20             schema:\n\
             \x20               type: object\n\
             \x20               properties:\n\
             \x20                 id:\n\
             \x20                   type: string\n\
             \x20                   readOnly: true\n\
             \x20                 name:\n\
             \x20                   type: string\n\
             \x20         description: Created",
        );
        let routes = extract_routes(&spec);
        assert!(routes[0].read_only_fields.contains("id"));
        assert!(!routes[0].read_only_fields.contains("name"));
    }

    // --- item_generator invocation ---

    #[test]
    fn extract_routes_item_generator_produces_a_value_matching_schema() {
        let spec = yaml(
            "paths:\n\
             \x20 /items:\n\
             \x20   get:\n\
             \x20     responses:\n\
             \x20       '200':\n\
             \x20         content:\n\
             \x20           application/json:\n\
             \x20             schema:\n\
             \x20               type: array\n\
             \x20               items:\n\
             \x20                 type: object\n\
             \x20                 properties:\n\
             \x20                   name:\n\
             \x20                     type: string\n\
             \x20                     example: hello\n\
             \x20         description: OK",
        );
        let routes = extract_routes(&spec);
        let r#gen = routes[0]
            .item_generator
            .as_ref()
            .expect("route should have an item_generator");
        let value = r#gen();
        assert!(value.is_object());
    }

    // --- circular $ref cycle detection ---

    #[test]
    fn flatten_all_of_returns_empty_when_all_of_is_not_a_sequence() {
        let root = yaml("{}");
        let schema = yaml("allOf: null");
        let flat = flatten_schema(&schema, &root);
        assert!(flat.is_mapping(), "flat schema should be a mapping value");
    }

    #[test]
    fn flatten_schema_does_not_overflow_on_direct_circular_ref() {
        let root = yaml("Node:\n  $ref: '#/Node'");
        let schema = yaml("$ref: '#/Node'");
        let flat = flatten_schema(&schema, &root);
        assert!(flat.is_mapping(), "flat schema should be a mapping value");
    }

    #[test]
    fn flatten_schema_does_not_overflow_on_indirect_circular_ref() {
        let root = yaml("A:\n  $ref: '#/B'\nB:\n  $ref: '#/A'");
        let schema = yaml("$ref: '#/A'");
        let flat = flatten_schema(&schema, &root);
        assert!(flat.is_mapping(), "flat schema should be a mapping value");
    }

    #[test]
    fn flatten_schema_all_of_with_circular_ref_in_item_does_not_overflow() {
        let root = yaml("Node:\n  $ref: '#/Node'");
        let schema = yaml("allOf:\n  - $ref: '#/Node'");
        let flat = flatten_schema(&schema, &root);
        assert!(flat.is_mapping(), "flat schema should be a mapping value");
    }

    #[test]
    fn find_discriminator_does_not_overflow_on_circular_ref() {
        let root = yaml("A:\n  $ref: '#/B'\nB:\n  $ref: '#/A'");
        let schema = yaml("$ref: '#/A'");
        let result = find_discriminator(&schema, &root);
        assert!(
            result.is_none(),
            "circular ref should produce no discriminator"
        );
    }

    // --- method field on extracted routes ---

    #[test]
    fn extract_routes_method_is_correct_for_route_without_response_schema() {
        let spec = yaml(
            "paths:\n\
             \x20 /items:\n\
             \x20   post:\n\
             \x20     responses:\n\
             \x20       '201':\n\
             \x20         description: Created",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes[0].method, HttpMethod::Post);
    }

    #[test]
    fn extract_routes_method_is_correct_for_route_with_response_schema() {
        let spec = yaml(
            "paths:\n\
             \x20 /items/{id}:\n\
             \x20   put:\n\
             \x20     responses:\n\
             \x20       '200':\n\
             \x20         content:\n\
             \x20           application/json:\n\
             \x20             schema:\n\
             \x20               type: object\n\
             \x20               properties:\n\
             \x20                 id:\n\
             \x20                   type: string\n\
             \x20                   example: abc\n\
             \x20         description: OK",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes[0].method, HttpMethod::Put);
    }

    // --- discriminator branch: status_code and read_only_fields ---

    #[test]
    fn extract_routes_discriminator_route_has_correct_status_code_and_read_only_fields() {
        let spec = yaml(
            "Cat:\n\
             \x20 type: object\n\
             \x20 properties:\n\
             \x20   id:\n\
             \x20     type: string\n\
             \x20     readOnly: true\n\
             \x20   kind:\n\
             \x20     type: string\n\
             \x20     example: cat\n\
             Dog:\n\
             \x20 type: object\n\
             \x20 properties:\n\
             \x20   kind:\n\
             \x20     type: string\n\
             \x20     example: dog\n\
             paths:\n\
             \x20 /items:\n\
             \x20   post:\n\
             \x20     responses:\n\
             \x20       '201':\n\
             \x20         content:\n\
             \x20           application/json:\n\
             \x20             schema:\n\
             \x20               oneOf:\n\
             \x20                 - $ref: '#/Cat'\n\
             \x20                 - $ref: '#/Dog'\n\
             \x20               discriminator:\n\
             \x20                 propertyName: kind\n\
             \x20                 mapping:\n\
             \x20                   cat: '#/Cat'\n\
             \x20                   dog: '#/Dog'\n\
             \x20         description: Created",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes[0].method, HttpMethod::Post);
        assert_eq!(routes[0].status_code, 201);
        assert!(routes[0].read_only_fields.contains("id"));
    }

    // --- item_generator for envelope responses ---

    #[test]
    fn extract_routes_item_generator_is_set_for_envelope_response_with_array_property() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let spec = yaml(
            "paths:\n\
             \x20 /items:\n\
             \x20   get:\n\
             \x20     responses:\n\
             \x20       '200':\n\
             \x20         content:\n\
             \x20           application/json:\n\
             \x20             schema:\n\
             \x20               type: object\n\
             \x20               properties:\n\
             \x20                 total:\n\
             \x20                   type: integer\n\
             \x20                   example: 5\n\
             \x20                 items:\n\
             \x20                   type: array\n\
             \x20                   items:\n\
             \x20                     type: object\n\
             \x20                     properties:\n\
             \x20                       name:\n\
             \x20                         type: string\n\
             \x20                         example: test\n\
             \x20         description: OK",
        );
        let routes = extract_routes(&spec);
        let generator = routes[0]
            .item_generator
            .as_ref()
            .expect("route should have an item_generator for envelope response");
        let value = generator();
        assert!(value.is_object(), "item generator should produce an object");
        assert_eq!(value["name"], serde_json::json!("test"));
    }

    // --- success response selection ---

    #[test]
    fn extract_routes_picks_any_2xx_response_code() {
        let spec = yaml(
            "paths:\n\
             \x20 /items:\n\
             \x20   get:\n\
             \x20     responses:\n\
             \x20       '206':\n\
             \x20         content:\n\
             \x20           application/json:\n\
             \x20             schema:\n\
             \x20               type: object\n\
             \x20               properties:\n\
             \x20                 name:\n\
             \x20                   type: string\n\
             \x20                   example: hello\n\
             \x20         description: Partial Content",
        );
        let routes = extract_routes(&spec);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].status_code, 206);
        assert!(
            routes[0].body.is_some(),
            "206 response should generate a body"
        );
    }
}

#[cfg(test)]
mod pagination_contract_tests {
    #[test]
    fn offset_pagination_requires_spec_parameters_and_envelope() {
        let source = r#"
openapi: 3.0.3
info: {title: Contract, version: '1'}
paths:
  /items:
    get:
      parameters:
        - {in: query, name: offset, schema: {type: integer}}
        - {in: query, name: limit, schema: {type: integer}}
      responses:
        '200':
          description: collection
          content:
            application/json:
              schema:
                type: object
                properties:
                  count: {type: integer}
                  next: {type: string, nullable: true}
                  previous: {type: string, nullable: true}
                  results: {type: array, items: {type: string}}
"#;
        for (source, expected) in [
            (source.to_owned(), true),
            (source.replace("name: offset", "name: cursor"), false),
            (
                source.replace("schema: {type: integer}", "schema: {type: string}"),
                false,
            ),
            (
                source.replace(
                    "next: {type: string, nullable: true}",
                    "other: {type: string}",
                ),
                false,
            ),
        ] {
            let spec = yaml_serde::from_str(&source).unwrap();
            let routes = super::extract_routes(&spec);
            assert_eq!(routes.len(), 1);
            assert_eq!(routes[0].offset_pagination, expected);
        }
    }
}
