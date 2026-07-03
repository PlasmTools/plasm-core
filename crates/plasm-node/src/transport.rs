//! Host transport bridge: route outbound HTTP through a NAPI threadsafe JS callback.

use async_trait::async_trait;
use napi::bindgen_prelude::*;
use napi::threadsafe_function::ThreadsafeFunction;
use plasm_compile::{CompiledRequest, HttpBodyFormat, HttpMethod};
use plasm_runtime::auth::ResolvedAuth;
use plasm_runtime::error::RuntimeError;
use plasm_runtime::http_transport::{compiled_http_url, HttpTransport};
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
        let js_promise =
            self.tsfn
                .call_async_catch(req)
                .await
                .map_err(|e| RuntimeError::RequestError {
                    message: format!("host transport callback failed: {e}"),
                    attempts: 1,
                })?;
        js_promise.await.map_err(|e| RuntimeError::RequestError {
            message: format!("host transport promise rejected: {e}"),
            attempts: 1,
        })
    }

    fn build_request(
        &self,
        method: &str,
        url: String,
        auth: Option<ResolvedAuth>,
        body: Option<String>,
    ) -> JsTransportRequest {
        let mut headers = HashMap::new();
        if let Some(a) = auth {
            for (key, value) in a.headers {
                if !key.trim().is_empty() && !value.trim().is_empty() {
                    headers.insert(key, value);
                }
            }
        }
        JsTransportRequest {
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
        resp: JsTransportResponse,
    ) -> std::result::Result<(serde_json::Value, Option<String>), RuntimeError> {
        if !(200..300).contains(&resp.status) {
            return Err(RuntimeError::RequestError {
                message: format!("HTTP {}: {}", resp.status, summarize_error_body(&resp.body)),
                attempts: 1,
            });
        }
        let json = serde_json::from_str(&resp.body)
            .unwrap_or_else(|_| serde_json::json!({ "content": resp.body }));
        Ok((json, resp.next_url))
    }
}

fn summarize_error_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.len() > 512 {
        format!("{}…", &trimmed[..512])
    } else {
        trimmed.to_string()
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

fn body_json_string(
    request: &CompiledRequest,
) -> std::result::Result<Option<String>, RuntimeError> {
    if request.body_format == HttpBodyFormat::Multipart {
        return Err(RuntimeError::ConfigurationError {
            message: "host transport callback does not support multipart bodies yet".into(),
        });
    }
    let Some(body) = &request.body else {
        return Ok(None);
    };
    serde_json::to_string(body)
        .map(Some)
        .map_err(|e| RuntimeError::SerializationError {
            message: format!("JSON encode outbound body: {e}"),
        })
}

#[async_trait]
impl HttpTransport for JsCallbackHttpTransport {
    async fn send_compiled_http(
        &self,
        base_url: &str,
        request: &CompiledRequest,
        auth: Option<ResolvedAuth>,
    ) -> std::result::Result<(serde_json::Value, Option<String>), RuntimeError> {
        let url = compiled_http_url(base_url, request);
        let method = compiled_method_label(&request.method);
        let body = body_json_string(request)?;
        let req = self.build_request(method, url, auth, body);
        let resp = self.invoke(req).await?;
        Self::parse_response(resp)
    }

    async fn get_json_absolute(
        &self,
        url: &str,
        auth: Option<ResolvedAuth>,
    ) -> std::result::Result<(serde_json::Value, Option<String>), RuntimeError> {
        let req = self.build_request("GET", url.to_string(), auth, None);
        let resp = self.invoke(req).await?;
        Self::parse_response(resp)
    }
}
