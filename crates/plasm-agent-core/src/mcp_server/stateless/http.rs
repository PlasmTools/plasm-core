//! Stateless POST `/mcp` axum handler (SEP-2575).

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use rust_mcp_sdk::auth::AuthInfo;
use rust_mcp_sdk::auth::AuthProvider;
use rust_mcp_sdk::mcp_server::server_runtime::create_server_instance;
use rust_mcp_sdk::schema::schema_utils::{ResultFromServer, ServerJsonrpcResponse};
use rust_mcp_sdk::schema::{
    ClientMessage, InitializeResult, JsonrpcErrorResponse, RequestId, RpcError,
};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::McpServerHandler;
use rust_mcp_sdk::ToMcpServerHandler;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::mcp_stream_auth::PlasmMcpApiKeyAuthProvider;
use crate::server_state::PlasmHostState;

use super::discover::build_discover_result;
use super::meta::{strip_transport_meta_from_params, validate_request_meta, RequestMetaError};

const MCP_PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
const MCP_METHOD_HEADER: &str = "mcp-method";
const MCP_NAME_HEADER: &str = "mcp-name";
const DISCOVER_METHOD: &str = "server/discover";

const REMOVED_METHODS: &[&str] = &[
    "initialize",
    "ping",
    "logging/setLevel",
    "resources/subscribe",
    "resources/unsubscribe",
    "subscriptions/listen",
];

#[derive(Clone)]
pub(crate) struct StatelessMcpState {
    plasm: Arc<PlasmHostState>,
    handler: Arc<dyn McpServerHandler>,
    server_details: Arc<InitializeResult>,
    auth: Option<Arc<PlasmMcpApiKeyAuthProvider>>,
}

/// Axum router: `GET /health`, `POST /mcp` (stateless).
pub(crate) async fn router(plasm: Arc<PlasmHostState>) -> axum::Router {
    let mut handler_struct = super::super::PlasmMcpHandler::new(Arc::clone(&plasm));
    if let Some(backend) = plasm.redis_backend.as_ref() {
        let plasm_redis = Arc::new(crate::mcp_transport_store::PlasmTransportRedisStore::new(
            Arc::clone(backend),
        ));
        handler_struct = handler_struct.with_transport_redis(plasm_redis);
    }
    let mcp_handler = handler_struct.to_mcp_server_handler();
    let server_details = Arc::new(super::super::mcp_stateless_server_details());

    let auth = if plasm.mcp_config_repository().is_some() || plasm.incoming_auth.is_some() {
        Some(Arc::new(PlasmMcpApiKeyAuthProvider::new(Arc::clone(
            &plasm,
        ))))
    } else {
        None
    };

    let state = StatelessMcpState {
        plasm: Arc::clone(&plasm),
        handler: mcp_handler,
        server_details,
        auth,
    };

    axum::Router::new()
        .route("/health", get(health))
        .route("/mcp", post(handle_post))
        .with_state(state)
        .layer(axum::middleware::from_fn(
            super::super::mcp_http_dns_rebinding::reject_dns_rebinding,
        ))
        .layer(axum::middleware::from_fn_with_state(
            plasm,
            super::super::mcp_http_user_agent::capture_mcp_http_user_agent,
        ))
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn handle_post(
    State(state): State<StatelessMcpState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/json"))
    {
        return json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            RpcError::invalid_request().with_message("Content-Type must be application/json"),
            None,
        );
    }

    let wire: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                RpcError::parse_error().with_message(e.to_string()),
                None,
            );
        }
    };

    let method = wire
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);
    let id = parse_request_id(wire.get("id"));
    let mut params = wire.get("params").cloned();

    let Some(method) = method else {
        return json_error(
            StatusCode::BAD_REQUEST,
            RpcError::invalid_request().with_message("missing JSON-RPC method"),
            id,
        );
    };

    if let Err(resp) = validate_sep_headers(&headers, &method, params.as_ref()) {
        return *resp;
    }

    let header_version = header_value(&headers, MCP_PROTOCOL_VERSION_HEADER);

    if method == DISCOVER_METHOD {
        return handle_discover(&state, &headers, params.as_ref(), id, header_version).await;
    }

    if REMOVED_METHODS.contains(&method.as_str()) {
        return json_error(
            StatusCode::NOT_FOUND,
            RpcError::method_not_found().with_message(format!("Method not found: {method}")),
            id,
        );
    }

    if !is_implemented_method(&method) {
        return json_error(
            StatusCode::NOT_FOUND,
            RpcError::method_not_found().with_message(format!("Method not found: {method}")),
            id,
        );
    }

    let client_init = match validate_request_meta(params.as_ref(), header_version) {
        Ok(v) => v,
        Err(e) => return meta_error_response(e, id),
    };

    if let Some(p) = params.as_mut() {
        strip_transport_meta_from_params(p);
    }

    let scrubbed_body = json!({
        "jsonrpc": "2.0",
        "id": wire.get("id").cloned().unwrap_or(Value::Null),
        "method": method,
        "params": params,
    });
    let scrubbed = scrubbed_body.to_string();

    let client_message = match scrubbed.parse::<ClientMessage>() {
        Ok(m) => m,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, e, id),
    };

    let ClientMessage::Request(request) = client_message else {
        return json_error(
            StatusCode::BAD_REQUEST,
            RpcError::invalid_request().with_message("expected JSON-RPC request"),
            id,
        );
    };

    let auth_info = match verify_auth(&state, &headers).await {
        Ok(info) => info,
        Err(resp) => return resp,
    };

    let session_id = format!("stateless-{}", Uuid::new_v4());
    if let Some(ua) = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        state
            .plasm
            .record_mcp_http_user_agent(&session_id, ua.to_string());
    }

    let runtime = create_server_instance(
        Arc::clone(&state.server_details),
        Arc::clone(&state.handler),
        session_id,
        auth_info,
        None,
        None,
        None,
    );

    if let Err(e) = runtime.set_client_details(client_init).await {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            RpcError::internal_error().with_message(e.to_string()),
            id,
        );
    }

    let request_id = request.request_id().clone();
    match state.handler.handle_request(request, runtime).await {
        Ok(result) => json_result(StatusCode::OK, request_id, result),
        Err(e) => json_error(status_for_rpc_error(&e), e, Some(request_id)),
    }
}

async fn handle_discover(
    state: &StatelessMcpState,
    headers: &HeaderMap,
    params: Option<&Value>,
    id: Option<RequestId>,
    header_version: Option<&str>,
) -> Response {
    if let Err(e) = validate_request_meta(params, header_version) {
        return meta_error_response(e, id);
    }

    if let Err(resp) = verify_auth(state, headers).await {
        return resp;
    }

    let result = build_discover_result(state.server_details.as_ref());
    let rid = id.unwrap_or(RequestId::Integer(0));
    let body = json!({
        "jsonrpc": "2.0",
        "id": request_id_to_json(&rid),
        "result": result,
    });
    (StatusCode::OK, Json(body)).into_response()
}

fn is_implemented_method(method: &str) -> bool {
    matches!(
        method,
        "tools/list"
            | "tools/call"
            | "resources/list"
            | "resources/templates/list"
            | "resources/read"
            | "prompts/list"
            | "prompts/get"
            | "completion/complete"
    )
}

fn validate_sep_headers(
    headers: &HeaderMap,
    method: &str,
    params: Option<&Value>,
) -> Result<(), Box<Response>> {
    if let Some(header_method) = header_value(headers, MCP_METHOD_HEADER) {
        if header_method != method {
            return Err(Box::new(json_error(
                StatusCode::BAD_REQUEST,
                RpcError::invalid_request()
                    .with_message("Mcp-Method header does not match body method"),
                None,
            )));
        }
    }
    if method == "tools/call" {
        if let Some(header_name) = header_value(headers, MCP_NAME_HEADER) {
            let body_name = params
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if header_name != body_name {
                return Err(Box::new(json_error(
                    StatusCode::BAD_REQUEST,
                    RpcError::invalid_request()
                        .with_message("Mcp-Name header does not match tools/call name"),
                    None,
                )));
            }
        }
    }
    Ok(())
}

async fn verify_auth(
    state: &StatelessMcpState,
    headers: &HeaderMap,
) -> Result<Option<AuthInfo>, Response> {
    let Some(auth) = state.auth.as_ref() else {
        return Ok(None);
    };
    let Some(token) = bearer_token(headers) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "invalid_token",
                "error_description": "Authorization: Bearer <token> required",
            })),
        )
            .into_response());
    };
    match auth.verify_token(token).await {
        Ok(info) => Ok(Some(info)),
        Err(e) => Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "invalid_token",
                "error_description": e.to_string(),
            })),
        )
            .into_response()),
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let value = value.trim();
    let rest = value.strip_prefix("Bearer ")?.trim();
    if rest.is_empty() {
        return None;
    }
    Some(rest.to_string())
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn parse_request_id(value: Option<&Value>) -> Option<RequestId> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    serde_json::from_value(value.clone()).ok()
}

fn request_id_to_json(id: &RequestId) -> Value {
    serde_json::to_value(id).unwrap_or(Value::Null)
}

fn meta_error_response(err: RequestMetaError, id: Option<RequestId>) -> Response {
    json_error(err.http_status(), err.into_rpc_error(), id)
}

fn json_result(status: StatusCode, id: RequestId, result: ResultFromServer) -> Response {
    let response = ServerJsonrpcResponse::new(id, result);
    let body = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn json_error(status: StatusCode, error: RpcError, id: Option<RequestId>) -> Response {
    let response = JsonrpcErrorResponse::new(error, id);
    let body = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn status_for_rpc_error(error: &RpcError) -> StatusCode {
    match error.code {
        -32602 | -32600 | -32020 | -32021 | -32019 => StatusCode::BAD_REQUEST,
        -32601 => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
