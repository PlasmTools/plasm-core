//! HTTP/GraphQL response narrowing and preprocess.

use super::*;

pub(crate) fn http_collection_source(cml: &CmlRequest) -> PathExpr {
    if let Some(ref r) = cml.response {
        if let Some(ref path) = r.items_path {
            if !path.is_empty() {
                let mut segs: Vec<PathSegment> =
                    path.iter().map(|name| items_path_segment(name)).collect();
                segs.push(PathSegment::Wildcard);
                if let Some(ref inner) = r.item_inner_key {
                    if !inner.is_empty() {
                        segs.push(PathSegment::Key {
                            name: inner.clone(),
                        });
                    }
                }
                return PathExpr::new(segs);
            }
        }
    }
    let key = cml.response_items_key().to_string();
    let mut segs = vec![PathSegment::Key { name: key }, PathSegment::Wildcard];
    if let Some(ref r) = cml.response {
        if let Some(ref inner) = r.item_inner_key {
            if !inner.is_empty() {
                segs.push(PathSegment::Key {
                    name: inner.clone(),
                });
            }
        }
    }
    PathExpr::new(segs)
}

/// `items_path` segments are usually object keys; digit-only strings address JSON array indices.
pub(crate) fn items_path_segment(name: &str) -> PathSegment {
    if let Ok(index) = name.parse::<usize>() {
        PathSegment::Index { index }
    } else {
        PathSegment::Key {
            name: name.to_string(),
        }
    }
}

/// Key used when normalizing a bare JSON array to `{ <key>: [...] }` (must match the leaf array name).
pub(crate) fn response_bare_array_wrap_key(cml: &CmlRequest) -> String {
    if let Some(ref r) = cml.response {
        if let Some(ref path) = r.items_path {
            if let Some(last) = path.last() {
                return last.clone();
            }
        }
    }
    cml.response_items_key().to_string()
}

/// If the template is HTTP or GraphQL, narrow the raw response to the entity-shaped JSON described
/// by CML `response.single` + `items_path`. Other transports (e.g. EVM) return `response` unchanged.
pub(crate) fn narrow_http_graphql_response_for_entity_decode(
    template: &CapabilityTemplate,
    response: serde_json::Value,
    env: &CmlEnv,
) -> Result<serde_json::Value, RuntimeError> {
    match template {
        CapabilityTemplate::CredentialBind(_) => Ok(response),
        CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => {
            preflight_command_envelope_for_single_entity_narrow(&response, cml)?;
            let response = match cml
                .response
                .as_ref()
                .and_then(|r| r.response_preprocess.as_ref())
            {
                Some(p) => apply_response_preprocess(response, cml, p, env),
                None => response,
            };
            extract_single_entity_payload_from_response(response, cml)
        }
        CapabilityTemplate::View(_) => Err(RuntimeError::ConfigurationError {
            message: "view capabilities do not use HTTP response narrowing".into(),
        }),
        CapabilityTemplate::EvmCall(_) | CapabilityTemplate::EvmLogs(_) => Ok(response),
    }
}

/// Fibery `/api/commands` returns `{ "success": bool, "result": … }` on HTTP 200 even when the
/// command failed. Surface `success: false` before callers treat the HTTP round-trip as success.
pub(crate) fn preflight_fibery_command_envelope(
    response: &serde_json::Value,
) -> Result<(), RuntimeError> {
    let Some(success) = response.get("success").and_then(|v| v.as_bool()) else {
        return Ok(());
    };
    let Some(result) = response.get("result") else {
        return Ok(());
    };
    if !success {
        let name = result
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("command.error");
        let message = result
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Fibery command failed");
        return Err(RuntimeError::RequestError {
            message: format!("Fibery command failed ({name}): {message}"),
            attempts: 1,
            status: None,
            body: None,
        });
    }
    Ok(())
}

/// Fibery `/api/commands` and similar APIs return `{ "success": bool, "result": … }` on HTTP 200.
/// Surface command failures and empty query rows before CML `items_path` narrowing turns them into
/// opaque "missing path segment" configuration errors.
pub(crate) fn preflight_command_envelope_for_single_entity_narrow(
    response: &serde_json::Value,
    cml: &CmlRequest,
) -> Result<(), RuntimeError> {
    let Some(r) = cml.response.as_ref().filter(|r| r.single) else {
        return Ok(());
    };
    preflight_fibery_command_envelope(response)?;
    if let Some(path) = r.items_path.as_ref().filter(|p| !p.is_empty()) {
        if let Some(msg) = graphql_mutation_envelope_failure(response, path) {
            return Err(RuntimeError::RequestError {
                message: msg,
                attempts: 1,
                status: None,
                body: None,
            });
        }
    }
    let Some(result) = response.get("result") else {
        return Ok(());
    };
    let Some(path) = r.items_path.as_ref().filter(|p| !p.is_empty()) else {
        return Ok(());
    };
    if path.len() < 2 || path[0] != "result" {
        return Ok(());
    }
    let Some(index_key) = path.get(1) else {
        return Ok(());
    };
    if index_key.parse::<usize>().is_err() {
        return Ok(());
    }
    if result.as_array().is_some_and(|a| a.is_empty()) {
        return Err(RuntimeError::RequestError {
            message: "Fibery command succeeded but returned no rows (empty `result` array). \
                      For `user_get_me`, the API token may not resolve `$my-id` — use a personal \
                      workspace API token from Fibery → API Tokens and reconnect in Plasm."
                .into(),
            attempts: 1,
            status: None,
            body: None,
        });
    }
    Ok(())
}

/// For mappings that declare `response.single` + `items_path` (e.g. GraphQL `{ data: { issue: { ... } } }`),
/// or `response.single` + top-level `items` (e.g. Cloudflare v4 `{ result: { ... } }` via `items: result`),
/// take the entity object at that path. Used for GET/detail, create, and update **invoke** decoding—
/// not specific to GET semantics.
pub(crate) fn extract_single_entity_payload_from_response(
    response: serde_json::Value,
    cml: &CmlRequest,
) -> Result<serde_json::Value, RuntimeError> {
    preflight_command_envelope_for_single_entity_narrow(&response, cml)?;
    if let Some(ref r) = cml.response {
        if r.single {
            let mut cur: &serde_json::Value = &response;
            if let Some(ref path) = r.items_path {
                if !path.is_empty() {
                    for (i, key) in path.iter().enumerate() {
                        cur = match response_path_step(cur, key) {
                            Some(v) => v,
                            None => {
                                let mut msg =
                                    format!("single-entity response: missing path segment `{key}`");
                                if let Some(gs) = graphql_errors_summary(&response) {
                                    msg.push_str(" — GraphQL: ");
                                    msg.push_str(&gs);
                                } else if matches!(response.get("data"), Some(d) if d.is_null()) {
                                    msg.push_str(
                                        " (response `data` is null; often paired with GraphQL `errors`)",
                                    );
                                } else if let Some(fibery) =
                                    fibery_command_envelope_hint(&response, key)
                                {
                                    msg.push_str(" — ");
                                    msg.push_str(&fibery);
                                }
                                return Err(RuntimeError::ConfigurationError { message: msg });
                            }
                        };
                        if cur.is_null() && i + 1 == path.len() {
                            let mut msg =
                                format!("Entity not found (`{key}` is null in the API response)");
                            if let Some(gs) = graphql_errors_summary(&response) {
                                msg.push_str(" — GraphQL: ");
                                msg.push_str(&gs);
                            }
                            return Err(RuntimeError::RequestError {
                                message: msg,
                                attempts: 1,
                                status: None,
                                body: None,
                            });
                        }
                    }
                }
            } else if let Some(key) = r.items.as_deref().filter(|k| !k.is_empty()) {
                cur = match single_response_path_step(cur, key) {
                    Some(v) => v,
                    None => {
                        let mut msg = format!("single-entity response: missing `{key}`");
                        if let Some(gs) = graphql_errors_summary(&response) {
                            msg.push_str(" — GraphQL: ");
                            msg.push_str(&gs);
                        }
                        return Err(RuntimeError::ConfigurationError { message: msg });
                    }
                };
            }
            let mut out = cur.clone();
            if let Some(ref inner) = r.item_inner_key {
                if !inner.is_empty() {
                    out = unwrap_single_inner_payload(out, inner)?;
                }
            }
            return Ok(out);
        }
    }
    Ok(response)
}

pub(crate) fn single_response_path_step<'a>(
    cur: &'a serde_json::Value,
    key: &str,
) -> Option<&'a serde_json::Value> {
    response_path_step(cur, key)
}

/// Reddit-style `{ kind, data: { … } }` wrappers and `{ children: [ { kind, data } ] }` listings:
/// unwrap the first child’s `data` when the value is a non-empty array; otherwise if the object
/// contains `inner`, return that subtree; else return the value unchanged.
pub(crate) fn unwrap_single_inner_payload(
    cur: serde_json::Value,
    inner: &str,
) -> Result<serde_json::Value, RuntimeError> {
    match cur {
        serde_json::Value::Array(mut a) => {
            let first = a.get_mut(0).map(std::mem::take).ok_or_else(|| {
                RuntimeError::ConfigurationError {
                    message: "single-entity response: expected a non-empty array at path"
                        .to_string(),
                }
            })?;
            match first {
                serde_json::Value::Object(m) => {
                    m.get(inner)
                        .cloned()
                        .ok_or_else(|| RuntimeError::ConfigurationError {
                            message: format!(
                                "single-entity response: array element missing `{inner}` object"
                            ),
                        })
                }
                _ => Err(RuntimeError::ConfigurationError {
                    message: "single-entity response: expected object elements in array"
                        .to_string(),
                }),
            }
        }
        serde_json::Value::Object(m) => {
            if let Some(v) = m.get(inner) {
                Ok(v.clone())
            } else {
                Ok(serde_json::Value::Object(m))
            }
        }
        other => Ok(other),
    }
}

/// Decode hints from CML: alternate `items` key (e.g. `meals`) and single-object bodies.
pub(crate) fn prepare_http_query_response(
    response: serde_json::Value,
    cml: &CmlRequest,
    env: &CmlEnv,
) -> serde_json::Value {
    let response = if let Some(ref r) = cml.response {
        if let Some(ref p) = r.response_preprocess {
            apply_response_preprocess(response, cml, p, env)
        } else {
            response
        }
    } else {
        response
    };
    let key = cml.response_items_key().to_string();
    if cml.response_is_single_object()
        && cml
            .response
            .as_ref()
            .is_none_or(|r| r.response_preprocess.is_none())
        && response.is_object()
        && !response.is_array()
    {
        return serde_json::Value::Object(
            std::iter::once((key.clone(), serde_json::json!([response]))).collect(),
        );
    }
    if cml.response.as_ref().is_some_and(|r| r.wrap_root_scalar)
        && matches!(
            &response,
            serde_json::Value::Number(_) | serde_json::Value::String(_)
        )
    {
        return serde_json::json!({ key: [response] });
    }
    // Root JSON array with `items_path` starting at an array index (e.g. Reddit
    // `/r/{sub}/comments/{id}.json` → [post_listing, comment_listing]): leave the body unchanged so
    // `http_collection_source` can walk into the second listing without wrapping the whole array.
    if response.is_array()
        && cml
            .response
            .as_ref()
            .and_then(|r| r.items_path.as_ref())
            .is_some_and(|p| !p.is_empty() && p[0].parse::<usize>().is_ok())
    {
        return response;
    }
    let wrap_key = response_bare_array_wrap_key(cml);
    normalize_collection_response(response, &wrap_key)
}

pub(crate) fn cml_id_string(want: &plasm_core::Value) -> String {
    if let Ok(v) = serde_json::to_value(want) {
        match v {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            _ => String::new(),
        }
    } else {
        String::new()
    }
}

pub(crate) fn wire_id_matches(maybe: &serde_json::Value, want: &plasm_core::Value) -> bool {
    if want == &plasm_core::Value::Null {
        return false;
    }
    let w = cml_id_string(want);
    if w.is_empty() {
        return false;
    }
    match maybe {
        serde_json::Value::String(s) => s == &w,
        serde_json::Value::Number(n) => n.to_string() == w,
        _ => false,
    }
}

fn collect_concat_array_source(
    response: &serde_json::Value,
    source: &ConcatArraySource,
) -> Option<Vec<serde_json::Value>> {
    let walked = walk_json_path(response, &source.path)?;
    match source.from_each.as_deref() {
        None => walked.as_array().cloned(),
        Some(field) => {
            let outer = walked.as_array()?;
            let mut acc = Vec::new();
            for it in outer {
                let Some(o) = it.as_object() else { continue };
                if let Some(serde_json::Value::Array(a)) = o.get(field) {
                    acc.extend(a.iter().cloned());
                }
            }
            Some(acc)
        }
    }
}

pub(crate) fn walk_json_path<'a>(
    v: &'a serde_json::Value,
    path: &[String],
) -> Option<&'a serde_json::Value> {
    let mut cur = v;
    for key in path {
        cur = if let Ok(i) = key.parse::<usize>() {
            cur.get(i)?
        } else {
            cur.get(key)?
        };
    }
    Some(cur)
}

pub(crate) fn get_mut_value_at_path<'a>(
    v: &'a mut serde_json::Value,
    path: &[String],
) -> Option<&'a mut serde_json::Value> {
    let mut cur = v;
    for key in path {
        if let Ok(i) = key.parse::<usize>() {
            let serde_json::Value::Array(a) = cur else {
                return None;
            };
            cur = a.get_mut(i)?;
        } else {
            let serde_json::Value::Object(o) = cur else {
                return None;
            };
            cur = o.get_mut(key)?;
        }
    }
    Some(cur)
}

pub(crate) fn apply_response_preprocess(
    response: serde_json::Value,
    cml: &CmlRequest,
    p: &ResponsePreprocess,
    env: &CmlEnv,
) -> serde_json::Value {
    let key = cml.response_items_key().to_string();
    match p {
        ResponsePreprocess::ObjectProjection { fields } => serde_json::Value::Object(
            fields
                .iter()
                .filter_map(|(name, path)| {
                    walk_json_path(&response, path).map(|value| (name.clone(), value.clone()))
                })
                .collect(),
        ),
        ResponsePreprocess::ArrayFindPluck {
            path,
            id_field,
            id_var,
            nested_array,
        } => {
            let want = match env.get(id_var) {
                Some(v) => v,
                None => return response,
            };
            let Some(serde_json::Value::Array(arr)) = walk_json_path(&response, path) else {
                return response;
            };
            for it in arr {
                let Some(obj) = it.as_object() else { continue };
                let Some(ida) = obj.get(id_field) else {
                    continue;
                };
                if !wire_id_matches(ida, want) {
                    continue;
                }
                if let Some(serde_json::Value::Array(pl)) = obj.get(nested_array) {
                    return serde_json::Value::Object(
                        std::iter::once((key, serde_json::Value::Array(pl.clone()))).collect(),
                    );
                }
            }
            serde_json::json!({ key: serde_json::Value::Array(vec![]) })
        }
        ResponsePreprocess::ConcatFieldArrays { path, from_each } => {
            let Some(serde_json::Value::Array(outer)) = walk_json_path(&response, path) else {
                return response;
            };
            let mut acc: Vec<serde_json::Value> = Vec::new();
            for it in outer {
                let Some(o) = it.as_object() else { continue };
                if let Some(serde_json::Value::Array(a)) = o.get(from_each) {
                    acc.extend(a.iter().cloned());
                }
            }
            serde_json::Value::Object(
                std::iter::once((key, serde_json::Value::Array(acc))).collect(),
            )
        }
        ResponsePreprocess::ConcatArrays { sources } => {
            let mut acc: Vec<serde_json::Value> = Vec::new();
            let mut any = false;
            for source in sources {
                let Some(rows) = collect_concat_array_source(&response, source) else {
                    continue;
                };
                any = true;
                acc.extend(rows);
            }
            if !any {
                return response;
            }
            serde_json::Value::Object(
                std::iter::once((key, serde_json::Value::Array(acc))).collect(),
            )
        }
        ResponsePreprocess::StringIdsToFieldObjects { path, field } => {
            if path.is_empty() {
                return response;
            }
            let mut out = response;
            if let Some(serde_json::Value::Array(a)) = get_mut_value_at_path(&mut out, path) {
                let fk = field.clone();
                let mapped: Vec<serde_json::Value> = a
                    .iter()
                    .filter_map(|v| {
                        v.as_str().map(|s| {
                            let mut o = serde_json::Map::new();
                            o.insert(fk.clone(), serde_json::Value::String(s.to_string()));
                            serde_json::Value::Object(o)
                        })
                    })
                    .collect();
                *a = mapped;
            }
            out
        }
    }
}

/// Normalize collection API responses: bare arrays become `{ items_field: [...] }`.
pub(crate) fn normalize_collection_response(
    response: serde_json::Value,
    items_field: &str,
) -> serde_json::Value {
    if response.is_array() {
        serde_json::json!({ items_field: response })
    } else {
        response
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn object_projection_preserves_wire_values(value in ".*") {
            let cml = CmlRequest::new(plasm_compile::HttpMethod::Post, vec![]);
            let projection = ResponsePreprocess::ObjectProjection {
                fields: [("path".into(), vec!["ack".into(), "file_path".into()]),
                         ("nullable".into(), vec!["nullable".into()]),
                         ("missing".into(), vec!["absent".into()])].into(),
            };
            let body = serde_json::json!({"ack":{"file_path":value},"nullable":null,"unrelated":"ignored"});
            let projected = apply_response_preprocess(body, &cml, &projection, &CmlEnv::new());
            prop_assert_eq!(projected, serde_json::json!({"path":value,"nullable":null}));
        }
    }
}
