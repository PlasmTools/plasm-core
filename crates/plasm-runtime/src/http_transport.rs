//! Pluggable HTTP transport for live backend calls.
//!
//! [`ExecutionEngine`](crate::ExecutionEngine) uses [`ReqwestHttpTransport`] by default;
//! swap in a custom [`HttpTransport`] for tests, proxies, or alternate clients.

use crate::api_error_detail::{
    sanitize_preview_chars, summarize_json_api_error_for_http, summarize_text_error_body,
    MAX_DEBUG_BODY_PREVIEW_CHARS,
};
use crate::auth::ResolvedAuth;
use crate::error::RuntimeError;
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64_ENGINE, Engine as _};
use plasm_compile::{
    CompiledMultipartBody, CompiledMultipartPart, CompiledRequest, HttpBodyFormat, HttpMethod,
};
use plasm_core::{Value, PLASM_ATTACHMENT_KEY};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, warn, Instrument};

/// Join `base_url` with the compiled request path.
///
/// Some APIs (e.g. PokéAPI) use a **full resource URL** as the stable entity id (`id_field: url`).
/// That value is interpolated into a path template like `/api/v2/evolution-chain/{id}/`, producing
/// `/api/v2/evolution-chain/https://host/.../5/` — concatenating `base_url` + that path would double
/// the origin. When `path` embeds an absolute `http://` or `https://` URL, return **only** that URL.
pub fn join_base_url_path(base_url: &str, path: &str) -> String {
    if let Some(idx) = path.find("https://") {
        return path[idx..].trim_end_matches('/').to_string();
    }
    if let Some(idx) = path.find("http://") {
        return path[idx..].trim_end_matches('/').to_string();
    }
    let base = base_url.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    format!("{}/{}", base, path)
}

/// Outbound HTTP: compile CML to request, then send and return JSON + optional `Link: rel=next` URL.
#[async_trait]
pub trait HttpTransport: Send + Sync {
    /// Send a compiled HTTP operation against `base_url` (no trailing slash).
    async fn send_compiled_http(
        &self,
        base_url: &str,
        request: &CompiledRequest,
        auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError>;

    /// GET an absolute URL (e.g. pagination `Link` continuation).
    async fn get_json_absolute(
        &self,
        url: &str,
        auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError>;
}

/// Default transport using [`reqwest::Client`].
#[derive(Debug, Clone)]
pub struct ReqwestHttpTransport {
    client: reqwest::Client,
}

impl ReqwestHttpTransport {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    pub fn into_shared(self) -> Arc<dyn HttpTransport> {
        Arc::new(self)
    }
}

fn apply_resolved_auth(
    mut req: reqwest::RequestBuilder,
    auth: Option<ResolvedAuth>,
) -> reqwest::RequestBuilder {
    if let Some(a) = auth {
        for (key, value) in a.headers {
            if key.trim().is_empty() || value.trim().is_empty() {
                continue;
            }
            req = req.header(key, value);
        }
        for (param, value) in a.query_params {
            if param.trim().is_empty() || value.trim().is_empty() {
                continue;
            }
            req = req.query(&[(param, value)]);
        }
    }
    req
}

#[async_trait]
impl HttpTransport for ReqwestHttpTransport {
    async fn send_compiled_http(
        &self,
        base_url: &str,
        request: &CompiledRequest,
        auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let url = join_base_url_path(base_url, request.url_path());
        let http_span =
            crate::spans::http_compiled_request(compiled_method_label(&request.method), url.len());

        let mut req_builder = match request.method {
            HttpMethod::Get => self.client.get(&url),
            HttpMethod::Post => self.client.post(&url),
            HttpMethod::Put => self.client.put(&url),
            HttpMethod::Patch => self.client.patch(&url),
            HttpMethod::Delete => self.client.delete(&url),
            HttpMethod::Head => self.client.head(&url),
            HttpMethod::Options => self.client.request(reqwest::Method::OPTIONS, &url),
        };

        if request.body_format == HttpBodyFormat::Multipart {
            let mp =
                request
                    .multipart
                    .as_ref()
                    .ok_or_else(|| RuntimeError::ConfigurationError {
                        message: "multipart request missing compiled multipart.parts".to_string(),
                    })?;
            let form = build_multipart_form(mp)?;
            req_builder = req_builder.multipart(form);
        } else if let Some(body) = &request.body {
            match request.body_format {
                HttpBodyFormat::Json => {
                    let json_body = plasm_value_to_json(body);
                    let stripped = strip_null_fields(json_body);
                    req_builder = req_builder.json(&stripped);
                }
                HttpBodyFormat::FormUrlencoded => {
                    let form = plasm_value_to_form_urlencoded(body)?;
                    req_builder = req_builder
                        .header(
                            reqwest::header::CONTENT_TYPE,
                            "application/x-www-form-urlencoded",
                        )
                        .body(form);
                }
                HttpBodyFormat::Multipart => {
                    return Err(RuntimeError::ConfigurationError {
                        message:
                            "multipart body_format requires compiled multipart.parts, not `body`"
                                .to_string(),
                    });
                }
            }
        }

        if let Some(query) = &request.query {
            let json_val = plasm_value_to_json(query);
            if let Some(obj) = json_val.as_object() {
                for (key, value) in obj {
                    match value {
                        serde_json::Value::Null => {}
                        serde_json::Value::Array(arr) => {
                            for elem in arr {
                                let s = elem
                                    .as_str()
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| elem.to_string());
                                req_builder = req_builder.query(&[(key.as_str(), s)]);
                            }
                        }
                        serde_json::Value::String(s) => {
                            req_builder = req_builder.query(&[(key.as_str(), s.as_str())]);
                        }
                        serde_json::Value::Number(n) => {
                            let s = n
                                .as_i64()
                                .map(|i| i.to_string())
                                .or_else(|| n.as_f64().map(|f| f.to_string()))
                                .unwrap_or_else(|| n.to_string());
                            req_builder = req_builder.query(&[(key.as_str(), s)]);
                        }
                        other => {
                            req_builder = req_builder.query(&[(key.as_str(), other.to_string())]);
                        }
                    }
                }
            }
        }

        req_builder = apply_resolved_auth(req_builder, auth);

        if let Some(headers) = &request.headers {
            let json_val = plasm_value_to_json(headers);
            if let Some(obj) = json_val.as_object() {
                for (key, value) in obj {
                    let header_val = match value {
                        serde_json::Value::Null => continue,
                        serde_json::Value::String(s) => {
                            if s.trim().is_empty() {
                                continue;
                            }
                            s.clone()
                        }
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        other => other.to_string(),
                    };
                    if header_val.trim().is_empty() {
                        continue;
                    }
                    // Template headers are applied **after** resolved auth so duplicate keys (notably
                    // `Authorization`) follow **last-wins** semantics at the reqwest layer.
                    req_builder = req_builder.header(key, header_val);
                }
            }
        }

        let started = Instant::now();
        let method = compiled_method_label(&request.method);
        let result = async {
            let response = req_builder.send().instrument(http_span).await?;
            parse_http_response(response, method).await
        }
        .await;
        crate::runtime_metrics::record_outbound_http_request(
            method,
            &url,
            result.is_ok(),
            started.elapsed(),
        );
        result
    }

    async fn get_json_absolute(
        &self,
        url: &str,
        auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let http_span = crate::spans::http_absolute_get(url.len());
        let req_builder = apply_resolved_auth(self.client.get(url), auth);

        let started = Instant::now();
        let result = async {
            let response = req_builder.send().instrument(http_span).await?;
            parse_http_response(response, "GET").await
        }
        .await;
        crate::runtime_metrics::record_outbound_http_request(
            "GET",
            url,
            result.is_ok(),
            started.elapsed(),
        );
        result
    }
}

fn compiled_method_label(m: &HttpMethod) -> &'static str {
    match m {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Head => "HEAD",
        HttpMethod::Options => "OPTIONS",
    }
}

/// When an HTTP **2xx** body is not JSON, coerce it into a single JSON object carrying
/// [`PLASM_ATTACHMENT_KEY`] with base64 bytes and a MIME taken from `Content-Type` when present.
/// Returns [`None`] when the body looks like HTML/XML so callers can keep the legacy JSON error.
fn synthetic_json_from_non_json_success_body(
    bytes: &[u8],
    content_type: Option<&str>,
) -> Option<serde_json::Value> {
    let preview = utf8_body_preview(bytes, 280);
    if body_preview_looks_like_markup(&preview) {
        return None;
    }
    let b64 = B64_ENGINE.encode(bytes);
    let mime = content_type
        .and_then(|ct| ct.split(';').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("application/octet-stream");
    Some(serde_json::json!({
        (PLASM_ATTACHMENT_KEY): {
            "bytes_base64": b64,
            "mime_type": mime,
        }
    }))
}

fn build_multipart_form(
    body: &CompiledMultipartBody,
) -> Result<reqwest::multipart::Form, RuntimeError> {
    use reqwest::multipart::Form;

    let mut form = Form::new();
    for part in &body.parts {
        form = add_multipart_part(form, part)?;
    }
    Ok(form)
}

fn add_multipart_part(
    form: reqwest::multipart::Form,
    spec: &CompiledMultipartPart,
) -> Result<reqwest::multipart::Form, RuntimeError> {
    use reqwest::multipart::Part;

    if spec.content.is_plasm_attachment_object() {
        let (bytes, mime_from_attach, file_from_attach) =
            plasm_attachment_bytes_for_multipart(&spec.content)?;
        let mut p = Part::bytes(bytes);
        let fname = spec
            .file_name
            .clone()
            .or(file_from_attach)
            .unwrap_or_else(|| "file".to_string());
        p = p.file_name(fname);
        let ct = spec
            .content_type
            .clone()
            .or(mime_from_attach)
            .unwrap_or_else(|| "application/octet-stream".to_string());
        p = p
            .mime_str(&ct)
            .map_err(|e| RuntimeError::ConfigurationError {
                message: format!(
                    "multipart part `{}` has invalid content_type `{ct}`: {e}",
                    spec.name
                ),
            })?;
        return Ok(form.part(spec.name.clone(), p));
    }

    if matches!(&spec.content, Value::Object(_) | Value::Array(_)) {
        let vec = serde_json::to_vec(&plasm_value_to_json(&spec.content)).map_err(|e| {
            RuntimeError::SerializationError {
                message: format!("multipart JSON encode for `{}`: {e}", spec.name),
            }
        })?;
        let mut p = Part::bytes(vec);
        let ct = spec
            .content_type
            .clone()
            .unwrap_or_else(|| "application/json".to_string());
        p = p
            .mime_str(&ct)
            .map_err(|e| RuntimeError::ConfigurationError {
                message: format!(
                    "multipart part `{}` has invalid content_type `{ct}`: {e}",
                    spec.name
                ),
            })?;
        if let Some(fname) = &spec.file_name {
            p = p.file_name(fname.clone());
        }
        return Ok(form.part(spec.name.clone(), p));
    }

    if spec.file_name.is_some() {
        return Err(RuntimeError::ConfigurationError {
            message: format!(
                "multipart part `{}`: file_name requires attachment (`__plasm_attachment`) or JSON object/array content",
                spec.name
            ),
        });
    }

    let text = match &spec.content {
        Value::PlasmInputRef(_) => {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "multipart part `{}`: compile-time Plasm input refs are not valid HTTP wire values",
                    spec.name
                ),
            });
        }
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Null => {
            return Err(RuntimeError::ConfigurationError {
                message: format!("multipart part `{}`: unexpected null content", spec.name),
            });
        }
        Value::Object(_) | Value::Array(_) => unreachable!("handled above"),
        Value::UnionCtor { .. } => {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "multipart part `{}`: union constructor values must be lowered before HTTP encode",
                    spec.name
                ),
            });
        }
    };

    if let Some(ct) = &spec.content_type {
        let mut p = Part::text(text);
        p = p
            .mime_str(ct)
            .map_err(|e| RuntimeError::ConfigurationError {
                message: format!(
                    "multipart part `{}` has invalid content_type `{ct}`: {e}",
                    spec.name
                ),
            })?;
        Ok(form.part(spec.name.clone(), p))
    } else {
        Ok(form.text(spec.name.clone(), text))
    }
}

type MultipartAttachmentPayload = (Vec<u8>, Option<String>, Option<String>);

/// Returns `(bytes, mime_type, file_name)` for outbound multipart. URI-only attachments are rejected.
fn plasm_attachment_bytes_for_multipart(
    v: &Value,
) -> Result<MultipartAttachmentPayload, RuntimeError> {
    let obj = v
        .as_object()
        .ok_or_else(|| RuntimeError::ConfigurationError {
            message: "multipart attachment value must be an object".to_string(),
        })?;
    let inner = obj
        .get(PLASM_ATTACHMENT_KEY)
        .and_then(|x| x.as_object())
        .ok_or_else(|| RuntimeError::ConfigurationError {
            message: "multipart file part expects `__plasm_attachment` metadata".to_string(),
        })?;

    let mime = inner
        .get("mime_type")
        .or_else(|| inner.get("media_type"))
        .and_then(|m| match m {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            _ => None,
        });

    let file_name = inner.get("file_name").and_then(|m| match m {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    });

    if let Some(Value::String(b64)) = inner.get("bytes_base64") {
        if b64.is_empty() {
            return Err(RuntimeError::ConfigurationError {
                message: "multipart attachment bytes_base64 is empty".to_string(),
            });
        }
        let bytes =
            B64_ENGINE
                .decode(b64.as_bytes())
                .map_err(|e| RuntimeError::ConfigurationError {
                    message: format!("multipart attachment base64 decode failed: {e}"),
                })?;
        return Ok((bytes, mime, file_name));
    }

    if inner.get("uri").is_some() {
        return Err(RuntimeError::ConfigurationError {
            message: "multipart file parts require `bytes_base64` in `__plasm_attachment` (uri-only attachments are not sent)"
                .to_string(),
        });
    }

    Err(RuntimeError::ConfigurationError {
        message: "multipart attachment must include non-empty `bytes_base64`".to_string(),
    })
}

fn plasm_value_to_form_urlencoded(body: &Value) -> Result<String, RuntimeError> {
    let m = body
        .as_object()
        .ok_or_else(|| RuntimeError::ConfigurationError {
            message: "form_urlencoded body must be a flat object of scalar fields".to_string(),
        })?;
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (k, v) in m {
        if matches!(v, Value::Null) {
            continue;
        }
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            _ => {
                return Err(RuntimeError::ConfigurationError {
                    message: format!(
                        "form_urlencoded body field `{k}` must be null or a scalar string/number/bool"
                    ),
                });
            }
        };
        pairs.push((k.clone(), s));
    }
    serde_urlencoded::to_string(pairs.as_slice()).map_err(|e| RuntimeError::SerializationError {
        message: format!("form_urlencoded encode failed: {e}"),
    })
}

/// Read full body, parse JSON, and surface non-2xx API errors.
async fn parse_http_response(
    response: reqwest::Response,
    method: &'static str,
) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
    let status = response.status();
    let url = response.url().to_string();
    let link = extract_link_next(response.headers());
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let bytes = response
        .bytes()
        .await
        .map_err(|e| RuntimeError::RequestError {
            message: format!("{method} {url} — failed to read response body: {e}"),
        })?;

    if bytes.is_empty() {
        return if status.is_success() {
            Ok((serde_json::Value::Null, link))
        } else {
            Err(RuntimeError::RequestError {
                message: format!("{method} {url} — HTTP {status} with empty body"),
            })
        };
    }

    let parse_result: Result<serde_json::Value, serde_json::Error> = serde_json::from_slice(&bytes);

    match (status.is_success(), parse_result) {
        (true, Ok(json)) => Ok((json, link)),
        (true, Err(e)) => {
            let preview = utf8_body_preview(&bytes, 280);
            let looks_like_markup = body_preview_looks_like_markup(&preview);
            warn!(
                target: "plasm_runtime::http",
                method,
                url = %url,
                status = %status,
                content_type = content_type.as_deref().unwrap_or("(none)"),
                body_len = bytes.len(),
                looks_like_markup,
                serde_err = %e,
                body_preview = %preview,
                "response body failed JSON parse"
            );
            if let Some(json) =
                synthetic_json_from_non_json_success_body(&bytes, content_type.as_deref())
            {
                debug!(
                    target: "plasm_runtime::http",
                    method,
                    url = %url,
                    status = %status,
                    content_type = content_type.as_deref().unwrap_or("(none)"),
                    body_len = bytes.len(),
                    "coerced non-JSON success body to synthetic __plasm_attachment JSON"
                );
                Ok((json, link))
            } else {
                Err(RuntimeError::RequestError {
                    message: format!(
                        "{method} {url} — HTTP {status}: response body is not valid JSON ({e}); content-type: {}; body preview: {preview}",
                        content_type.as_deref().unwrap_or("(none)")
                    ),
                })
            }
        }
        (false, Ok(json)) => {
            let detail = summarize_json_api_error_for_http(&json);
            Err(RuntimeError::RequestError {
                message: format!("{method} {url} — HTTP {status} from API: {detail}"),
            })
        }
        (false, Err(parse_err)) => {
            let preview = utf8_body_preview(&bytes, MAX_DEBUG_BODY_PREVIEW_CHARS);
            debug!(
                target: "plasm_runtime::http",
                method,
                url = %url,
                status = %status,
                content_type = content_type.as_deref().unwrap_or("(none)"),
                body_len = bytes.len(),
                serde_err = %parse_err,
                body_preview = %preview,
                "non-success response body is not JSON; using bounded text summary"
            );
            let detail = summarize_text_error_body(&bytes, content_type.as_deref());
            Err(RuntimeError::RequestError {
                message: format!("{method} {url} — HTTP {status} from API: {detail}"),
            })
        }
    }
}

/// For tracing only — coarse signal when debugging replay / wrong content-type.
fn body_preview_looks_like_markup(preview: &str) -> bool {
    let p = preview.trim_start();
    p.starts_with("<!DOCTYPE")
        || p.starts_with("<!doctype")
        || p.starts_with("<html")
        || p.starts_with("<?xml")
}

fn utf8_body_preview(bytes: &[u8], max_chars: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    let t = s.trim();
    let truncated: String = if t.chars().count() > max_chars {
        t.chars().take(max_chars).collect()
    } else {
        t.to_string()
    };
    sanitize_preview_chars(&truncated)
}

fn extract_link_next(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let raw = headers
        .get(reqwest::header::LINK)
        .or_else(|| headers.get("link"))
        .and_then(|v| v.to_str().ok())?;

    for part in raw.split(',') {
        let part = part.trim();
        let is_next = part.contains("rel=\"next\"")
            || part.contains("rel='next'")
            || part.contains("rel=next");
        if !is_next {
            continue;
        }
        let start = part.find('<')?;
        let end = part.find('>')?;
        let url = part[start + 1..end].trim();
        if !url.is_empty() {
            return Some(url.to_string());
        }
    }
    None
}

fn strip_null_fields(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let filtered = map
                .into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_null_fields(v)))
                .collect();
            serde_json::Value::Object(filtered)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(strip_null_fields).collect())
        }
        other => other,
    }
}

fn plasm_value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::PlasmInputRef(_) => serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Integer(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(plasm_value_to_json).collect())
        }
        Value::Object(obj) => {
            let mut map = serde_json::Map::new();
            for (k, v) in obj {
                map.insert(k.clone(), plasm_value_to_json(v));
            }
            serde_json::Value::Object(map)
        }
        Value::UnionCtor { .. } => serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
    }
}

#[cfg(test)]
mod join_base_url_path_tests {
    use super::join_base_url_path;

    #[test]
    fn concatenates_normal_paths() {
        assert_eq!(
            join_base_url_path("https://pokeapi.co", "/api/v2/pokemon/1/"),
            "https://pokeapi.co/api/v2/pokemon/1/"
        );
    }

    #[test]
    fn uses_embedded_absolute_https_url_pokeapi_evolution_chain_id() {
        let path = "/api/v2/evolution-chain/https://pokeapi.co/api/v2/evolution-chain/5/";
        assert_eq!(
            join_base_url_path("https://pokeapi.co", path),
            "https://pokeapi.co/api/v2/evolution-chain/5"
        );
    }
}

#[cfg(test)]
mod strip_null_fields_tests {
    use super::strip_null_fields;

    /// GraphQL over HTTP: `variables.input` often carries optional mutation fields as null
    /// internally; strip removes those keys so the wire body omits them (partial input).
    #[test]
    fn nested_graphql_variables_input_omits_null_optional_fields() {
        let body = serde_json::json!({
            "query": "mutation($id: String!, $input: IssueUpdateInput!) { issueUpdate(id: $id, input: $input) { success } }",
            "variables": {
                "id": "issue-uuid",
                "input": {
                    "title": "Renamed",
                    "stateId": null,
                    "assigneeId": null,
                    "parentId": null
                }
            }
        });
        let stripped = strip_null_fields(body);
        let input = stripped["variables"]["input"]
            .as_object()
            .expect("input object");
        assert_eq!(input.get("title").and_then(|v| v.as_str()), Some("Renamed"));
        assert!(!input.contains_key("stateId"));
        assert!(!input.contains_key("assigneeId"));
        assert!(!input.contains_key("parentId"));
    }

    #[test]
    fn preserves_explicit_string_values_under_nested_keys() {
        let body = serde_json::json!({
            "variables": {
                "input": {
                    "teamId": "team-1",
                    "stateId": "ws-99"
                }
            }
        });
        let stripped = strip_null_fields(body);
        assert_eq!(
            stripped["variables"]["input"]["stateId"].as_str(),
            Some("ws-99")
        );
    }
}

#[cfg(test)]
mod multipart_wire_tests {
    use super::build_multipart_form;
    use indexmap::IndexMap;
    use plasm_compile::{CompiledMultipartBody, CompiledMultipartPart};
    use plasm_core::{Value, PLASM_ATTACHMENT_KEY};
    use reqwest::header::CONTENT_TYPE;

    #[test]
    fn multipart_request_sets_form_data_content_type() {
        let mut inner = IndexMap::new();
        inner.insert("bytes_base64".into(), Value::String("QUJD".into()));
        inner.insert("mime_type".into(), Value::String("image/png".into()));
        let mut outer = IndexMap::new();
        outer.insert(PLASM_ATTACHMENT_KEY.into(), Value::Object(inner));
        let body = CompiledMultipartBody {
            parts: vec![
                CompiledMultipartPart {
                    name: "meta".into(),
                    file_name: None,
                    content_type: None,
                    content: Value::String("x".into()),
                },
                CompiledMultipartPart {
                    name: "file".into(),
                    file_name: Some("p.png".into()),
                    content_type: None,
                    content: Value::Object(outer),
                },
            ],
        };
        let form = build_multipart_form(&body).expect("form");
        let client = reqwest::Client::new();
        let req = client
            .post("http://127.0.0.1:9/upload")
            .multipart(form)
            .build()
            .expect("build");
        let ct = req.headers().get(CONTENT_TYPE).expect("content-type");
        let s = ct.to_str().expect("utf8");
        assert!(
            s.starts_with("multipart/form-data"),
            "unexpected Content-Type: {s}"
        );
        assert!(s.contains("boundary="), "boundary in {s}");
    }
}

#[cfg(test)]
mod synthetic_attachment_tests {
    use super::synthetic_json_from_non_json_success_body;
    use plasm_core::PLASM_ATTACHMENT_KEY;

    #[test]
    fn wraps_binary_payload_when_not_markup() {
        let bytes: Vec<u8> = vec![0, 1, 2, 255];
        let v = synthetic_json_from_non_json_success_body(
            &bytes,
            Some("application/pdf; charset=binary"),
        )
        .expect("some");
        let inner = v[PLASM_ATTACHMENT_KEY].as_object().expect("inner");
        assert_eq!(inner["mime_type"], "application/pdf");
        assert!(!inner["bytes_base64"].as_str().unwrap_or("").is_empty());
    }

    #[test]
    fn rejects_html_like_bodies() {
        let html = b"<!DOCTYPE html><html><body>x</body></html>";
        assert!(synthetic_json_from_non_json_success_body(html, Some("text/html")).is_none());
    }
}
