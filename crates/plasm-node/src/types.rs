use napi_derive::napi;
use std::collections::HashMap;

#[napi(object)]
#[derive(Clone, Debug)]
pub struct JsTransportRequest {
    /// Scoped credentials require the callback to reject HTTP redirects.
    pub reject_redirects: bool,
    pub require_host_auth: bool,
    pub method: String,
    pub url: String,
    pub headers: Option<HashMap<String, String>>,
    pub body: Option<String>,
    pub entry_id: Option<String>,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct JsTransportResponse {
    pub status: u16,
    pub body: String,
    #[napi(js_name = "nextUrl")]
    pub next_url: Option<String>,
}
