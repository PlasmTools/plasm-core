//! Host transport bridge: route outbound HTTP through a NAPI threadsafe JS callback.
//!
//! Body encoding must match MCP/`ReqwestHttpTransport`: JSON vs `form_urlencoded`
//! (and multipart rejected until supported). Never force JSON for form bodies.

use async_trait::async_trait;
use napi::bindgen_prelude::*;
use napi::threadsafe_function::ThreadsafeFunction;
use plasm_compile::{CompiledRequest, HttpBodyFormat, HttpMethod};
use plasm_runtime::auth::ResolvedAuth;
use plasm_runtime::error::RuntimeError;
use plasm_runtime::http_transport::{
    compiled_http_url, compiled_template_headers, plasm_value_to_form_urlencoded, HttpTransport,
};
use plasm_runtime::request_error_from_host_http;
use std::collections::HashMap;
use std::sync::Arc;

use crate::types::{JsTransportRequest, JsTransportResponse};

pub type TransportTsfn = ThreadsafeFunction<
    JsTransportRequest,
    Promise<JsTransportResponse>,
    JsTransportRequest,
    napi::Status,
    false,
>;

pub fn build_transport_tsfn(
    callback: Function<'static, JsTransportRequest, Promise<JsTransportResponse>>,
) -> Result<TransportTsfn> {
    callback
        .build_threadsafe_function::<JsTransportRequest>()
        .build()
}

/// Host transport callback converted synchronously at the NAPI boundary (`Send` for async Rust).
pub struct JsHostTransport(pub TransportTsfn);

impl TypeName for JsHostTransport {
    fn type_name() -> &'static str {
        "Function"
    }

    fn value_type() -> napi::ValueType {
        napi::ValueType::Function
    }
}

impl ValidateNapiValue for JsHostTransport {
    unsafe fn validate(
        env: napi::sys::napi_env,
        napi_val: napi::sys::napi_value,
    ) -> Result<napi::sys::napi_value> {
        Function::<JsTransportRequest, JsTransportResponse>::validate(env, napi_val)
    }
}

impl FromNapiValue for JsHostTransport {
    unsafe fn from_napi_value(
        env: napi::sys::napi_env,
        napi_val: napi::sys::napi_value,
    ) -> Result<Self> {
        let callback =
            Function::<JsTransportRequest, Promise<JsTransportResponse>>::from_napi_value(
                env, napi_val,
            )?;
        let tsfn = build_transport_tsfn(callback)?;
        Ok(JsHostTransport(tsfn))
    }
}

#[derive(Clone)]
pub struct JsCallbackHttpTransport {
    tsfn: Arc<TransportTsfn>,
    entry_id: Option<String>,
}

impl JsCallbackHttpTransport {
    pub fn new(tsfn: TransportTsfn, entry_id: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            tsfn: Arc::new(tsfn),
            entry_id,
        })
    }

    async fn invoke(
        &self,
        req: JsTransportRequest,
    ) -> std::result::Result<JsTransportResponse, RuntimeError> {
        tracing::debug!(target: "plasm_node::transport", method = %req.method, "dispatch JS transport callback");
        let js_promise =
            self.tsfn
                .call_async_catch(req)
                .await
                .map_err(|e| RuntimeError::RequestError {
                    message: format!("host transport callback failed: {e}"),
                    attempts: 1,
                    status: None,
                    body: None,
                })?;
        js_promise.await.map_err(|e| RuntimeError::RequestError {
            message: format!("host transport promise rejected: {e}"),
            attempts: 1,
            status: None,
            body: None,
        })
    }

    fn build_request(
        &self,
        method: &str,
        url: String,
        auth: Option<ResolvedAuth>,
        body: Option<String>,
        content_type: Option<&str>,
        template_headers: Vec<(String, String)>,
    ) -> JsTransportRequest {
        let mut headers = HashMap::new();
        for (key, value) in template_headers {
            if !key.trim().is_empty() && !value.trim().is_empty() {
                headers.insert(key, value);
            }
        }
        if let Some(a) = auth {
            for (key, value) in a.headers {
                if !key.trim().is_empty() && !value.trim().is_empty() {
                    headers.insert(key, value);
                }
            }
        }
        if let Some(ct) = content_type {
            headers.insert("content-type".into(), ct.into());
        }
        JsTransportRequest {
            reject_redirects: false,
            require_host_auth: false,
            method: method.to_string(),
            url,
            headers: if headers.is_empty() {
                None
            } else {
                Some(headers)
            },
            body,
            entry_id: self.entry_id.clone(),
        }
    }

    fn parse_response(
        method: &str,
        url: &str,
        authorization_header: Option<&str>,
        resp: JsTransportResponse,
    ) -> std::result::Result<(serde_json::Value, Option<String>), RuntimeError> {
        plasm_runtime::http_transport::trace_hydration_http_response(resp.status, resp.body.len());
        if !(200..300).contains(&resp.status) {
            return Err(request_error_from_host_http(
                method,
                url,
                authorization_header,
                resp.status,
                &resp.body,
            ));
        }
        let json = serde_json::from_str(&resp.body)
            .unwrap_or_else(|_| serde_json::json!({ "content": resp.body }));
        Ok((json, resp.next_url))
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

/// Encode outbound body for the JS host callback — same law as MCP reqwest transport.
fn encode_outbound_body(
    request: &CompiledRequest,
) -> std::result::Result<Option<(String, &'static str)>, RuntimeError> {
    match request.body_format {
        HttpBodyFormat::Multipart => Err(RuntimeError::ConfigurationError {
            message: "host transport callback does not support multipart bodies yet".into(),
        }),
        HttpBodyFormat::Json => {
            let Some(body) = &request.body else {
                return Ok(None);
            };
            let encoded =
                serde_json::to_string(body).map_err(|e| RuntimeError::SerializationError {
                    message: format!("JSON encode outbound body: {e}"),
                })?;
            Ok(Some((encoded, "application/json; charset=utf-8")))
        }
        HttpBodyFormat::FormUrlencoded => {
            let Some(body) = &request.body else {
                return Ok(None);
            };
            let encoded = plasm_value_to_form_urlencoded(body)?;
            Ok(Some((encoded, "application/x-www-form-urlencoded")))
        }
    }
}

#[async_trait]
impl HttpTransport for JsCallbackHttpTransport {
    fn injects_host_auth(&self) -> bool {
        true
    }
    async fn send_compiled_http(
        &self,
        base_url: &str,
        request: &CompiledRequest,
        auth: Option<ResolvedAuth>,
    ) -> std::result::Result<(serde_json::Value, Option<String>), RuntimeError> {
        let url = compiled_http_url(base_url, request);
        let method = compiled_method_label(&request.method);
        let (body, content_type) = match encode_outbound_body(request)? {
            Some((b, ct)) => (Some(b), Some(ct)),
            None => (None, None),
        };
        let template_headers = compiled_template_headers(request, auth.as_ref())?;
        let require_host_auth = request.credential.is_some()
            && !auth.as_ref().is_some_and(|auth| {
                auth.headers
                    .iter()
                    .chain(&auth.query_params)
                    .any(|(_, value)| !value.trim().is_empty())
            });
        let mut req = self.build_request(method, url, auth, body, content_type, template_headers);
        req.reject_redirects = request.credential.is_some();
        req.require_host_auth = require_host_auth;
        let authorization = req.headers.as_ref().and_then(|headers| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.clone())
        });
        let resp = self.invoke(req).await?;
        plasm_runtime::http_transport::trace_compiled_http_boundary(
            request,
            method,
            resp.status,
            resp.body.as_bytes(),
            None,
            &plasm_runtime::http_auth_failure::OutboundAuthorizationFact::from_header(
                authorization.as_deref(),
            ),
        );
        Self::parse_response(
            method,
            &compiled_http_url(base_url, request),
            authorization.as_deref(),
            resp,
        )
    }

    async fn get_json_absolute(
        &self,
        url: &str,
        auth: Option<ResolvedAuth>,
    ) -> std::result::Result<(serde_json::Value, Option<String>), RuntimeError> {
        let req = self.build_request("GET", url.to_string(), auth, None, None, Vec::new());
        self.invoke_and_parse(req).await
    }
}

impl JsCallbackHttpTransport {
    async fn invoke_and_parse(
        &self,
        req: JsTransportRequest,
    ) -> std::result::Result<(serde_json::Value, Option<String>), RuntimeError> {
        let method = req.method.clone();
        let url = req.url.clone();
        let authorization = req.headers.as_ref().and_then(|headers| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.clone())
        });
        let resp = self.invoke(req).await?;
        Self::parse_response(&method, &url, authorization.as_deref(), resp)
    }
}

#[cfg(test)]
mod tests {
    use super::encode_outbound_body;
    use indexmap::IndexMap;
    use plasm_compile::{CompiledRequest, HttpBodyFormat, HttpMethod};
    use plasm_core::Value;

    fn base_request(format: HttpBodyFormat, body: Option<Value>) -> CompiledRequest {
        CompiledRequest {
            credential: None,
            method: HttpMethod::Post,
            path: "/auth/token".into(),
            query: None,
            body,
            body_format: format,
            multipart: None,
            headers: None,
        }
    }

    #[test]
    fn form_urlencoded_encodes_scalar_pairs_not_json() {
        let mut fields = IndexMap::new();
        fields.insert("username".into(), Value::String("joyce@x.com".into()));
        fields.insert("password".into(), Value::String("s3cret".into()));
        let req = base_request(HttpBodyFormat::FormUrlencoded, Some(Value::Object(fields)));
        let (body, ct) = encode_outbound_body(&req).expect("encode").expect("body");
        assert_eq!(ct, "application/x-www-form-urlencoded");
        assert!(
            !body.starts_with('{'),
            "must not JSON-encode form body: {body}"
        );
        assert!(body.contains("username=joyce"), "{body}");
        assert!(body.contains("password=s3cret"), "{body}");
    }

    #[test]
    fn json_body_sets_json_content_type() {
        let mut fields = IndexMap::new();
        fields.insert("ok".into(), Value::Bool(true));
        let req = base_request(HttpBodyFormat::Json, Some(Value::Object(fields)));
        let (body, ct) = encode_outbound_body(&req).expect("encode").expect("body");
        assert_eq!(ct, "application/json; charset=utf-8");
        assert!(
            body.contains("\"ok\":true") || body.contains("\"ok\": true"),
            "{body}"
        );
    }

    #[test]
    fn host_401_bearer_present_names_wire_hides_token() {
        const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1c2VyIn0.signatureTAIL";
        let err = plasm_runtime::request_error_from_host_http(
            "GET",
            "https://api.example.com/users?query=alice",
            Some(&format!("Bearer {JWT}")),
            401,
            r#"{"message":"Invalid credentials"}"#,
        );
        let message = err.to_string();
        assert!(
            message.contains("GET path=/users query=query=alice"),
            "{message}"
        );
        assert!(message.contains("Authorization: present"), "{message}");
        assert!(!message.contains(JWT), "full JWT leaked: {message}");
    }

    #[test]
    fn host_401_authorization_absent() {
        let err = plasm_runtime::request_error_from_host_http(
            "POST",
            "https://api.example.com/notes",
            None,
            401,
            r#"{"message":"missing token"}"#,
        );
        let message = err.to_string();
        assert!(
            message.contains("POST path=/notes query=(none)"),
            "{message}"
        );
        assert!(message.contains("Authorization: absent"), "{message}");
        assert!(!message.contains("tail"), "{message}");
    }
}
