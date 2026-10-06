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
    /// Callbacks and their HTTP clients must preserve supplied credentials.
    /// When Authorization is supplied, they MUST NOT add or replace any auth
    /// material (including Cookie, API-key headers or query credentials).
    /// Host bearer injection may occur only when Authorization is absent.
    /// Adapters requiring hidden credentials cannot use this engine-visible
    /// contract: they must attest TransportAuthScope::Opaque, causing typed
    /// RuntimeError::RequestIdentityAuthOpaque for request-owned observations.
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
