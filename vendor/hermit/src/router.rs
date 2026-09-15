use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::{
    Json, Router,
    extract::{MatchedPath, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::MethodRouter,
};

use crate::cli::CorsOrigins;
use crate::constants::{DEFAULT_MAX_ITEMS, DEFAULT_MIN_ITEMS};
use crate::http_method::HttpMethod;
use crate::resource_generator::use_examples;
use crate::resource_store::{
    CrudStore, build_collection_response, extract_items_from_mock, fill_to_count,
    fill_to_count_with_generator, is_item_pattern, json_value_to_string, new_id_like, new_uuid,
};
use crate::spec_parser::RouteConfig;

#[derive(Clone)]
pub struct AppState {
    routes: HashMap<String, RouteConfig>,
    store: Arc<RwLock<CrudStore>>,
    collection_templates: HashMap<String, Vec<serde_json::Value>>,
    item_generators: HashMap<String, Arc<dyn Fn() -> serde_json::Value + Send + Sync>>,
    min_items: usize,
    max_items: usize,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            routes: HashMap::new(),
            store: Arc::new(RwLock::new(CrudStore::new())),
            collection_templates: HashMap::new(),
            item_generators: HashMap::new(),
            min_items: DEFAULT_MIN_ITEMS,
            max_items: DEFAULT_MAX_ITEMS,
        }
    }
}

pub fn with_cors(router: Router, origins: &CorsOrigins) -> Router {
    let cors = match origins {
        CorsOrigins::All => tower_http::cors::CorsLayer::permissive(),
        CorsOrigins::List(list) => {
            let values: Vec<axum::http::HeaderValue> = list
                .iter()
                .map(|o| o.parse().expect("origin should be a valid header value"))
                .collect();
            tower_http::cors::CorsLayer::new()
                .allow_origin(tower_http::cors::AllowOrigin::list(values))
                .allow_methods(tower_http::cors::Any)
                .allow_headers(tower_http::cors::Any)
        }
    };
    router.layer(cors)
}

pub fn build(configs: Vec<RouteConfig>) -> Router {
    build_with_bounds(configs, DEFAULT_MIN_ITEMS, DEFAULT_MAX_ITEMS)
}

pub fn build_with_bounds(configs: Vec<RouteConfig>, min_items: usize, max_items: usize) -> Router {
    let mut by_path: HashMap<String, MethodRouter<Arc<AppState>>> = HashMap::new();
    for cfg in &configs {
        let mr = method_router_for(&cfg.method);
        let path = cfg.axum_path.clone();
        by_path
            .entry(path)
            .and_modify(|existing| {
                let combined = std::mem::replace(existing, MethodRouter::new()).merge(mr.clone());
                *existing = combined;
            })
            .or_insert(mr);
    }

    let mut collection_templates: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut item_generators: HashMap<String, Arc<dyn Fn() -> serde_json::Value + Send + Sync>> =
        HashMap::new();
    for cfg in &configs {
        register_collection_entry(cfg, &mut collection_templates, &mut item_generators);
    }

    let state = Arc::new(AppState {
        routes: into_state_map(configs),
        store: Arc::new(RwLock::new(CrudStore::new())),
        collection_templates,
        item_generators,
        min_items,
        max_items,
    });

    let mut router = Router::new();
    for (path, mr) in by_path {
        router = router.route(&path, mr);
    }
    router.with_state(state)
}

/// Serve the response bodies generated from OpenAPI without CRUD inference.
/// Request fields and path identities never manufacture response properties.
/// This mode is intended for client/catalog response conformance checks.
pub fn build_spec_responses(configs: Vec<RouteConfig>) -> Router {
    let mut by_path: HashMap<String, MethodRouter<Arc<AppState>>> = HashMap::new();
    for cfg in &configs {
        let mr = match cfg.method {
            HttpMethod::Get => axum::routing::get(handle_spec_response),
            HttpMethod::Post => axum::routing::post(handle_spec_response),
            HttpMethod::Put => axum::routing::put(handle_spec_response),
            HttpMethod::Patch => axum::routing::patch(handle_spec_response),
            HttpMethod::Delete => axum::routing::delete(handle_spec_response),
            HttpMethod::Options => axum::routing::options(handle_spec_response),
            HttpMethod::Head => axum::routing::head(handle_spec_response),
            HttpMethod::Trace => axum::routing::trace(handle_spec_response),
        };
        by_path
            .entry(cfg.axum_path.clone())
            .and_modify(|existing| {
                *existing = std::mem::replace(existing, MethodRouter::new()).merge(mr.clone());
            })
            .or_insert(mr);
    }
    let state = Arc::new(AppState {
        routes: into_state_map(configs),
        ..Default::default()
    });
    let mut router = Router::new();
    for (path, methods) in by_path {
        router = router.route(&path, methods);
    }
    router.with_state(state)
}

async fn handle_spec_response(
    State(state): State<Arc<AppState>>,
    matched: MatchedPath,
    method: Method,
) -> Response {
    match state
        .routes
        .get(&route_key(method.as_str(), matched.as_str()))
    {
        Some(cfg) => json_response(cfg.status_code, cfg.body.clone()),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn method_router_for(method: &HttpMethod) -> MethodRouter<Arc<AppState>> {
    match method {
        HttpMethod::Get => axum::routing::get(handle_readonly),
        HttpMethod::Delete => axum::routing::delete(handle_readonly),
        HttpMethod::Options => axum::routing::options(handle_readonly),
        HttpMethod::Head => axum::routing::head(handle_readonly),
        HttpMethod::Trace => axum::routing::trace(handle_readonly),
        HttpMethod::Post => axum::routing::post(handle_with_body),
        HttpMethod::Put => axum::routing::put(handle_with_body),
        HttpMethod::Patch => axum::routing::patch(handle_with_body),
    }
}

fn register_collection_entry(
    cfg: &RouteConfig,
    collection_templates: &mut HashMap<String, Vec<serde_json::Value>>,
    item_generators: &mut HashMap<String, Arc<dyn Fn() -> serde_json::Value + Send + Sync>>,
) {
    if cfg.method == HttpMethod::Get && !is_item_pattern(&cfg.axum_path) {
        collection_templates.insert(cfg.axum_path.clone(), extract_items_from_mock(&cfg.body));
        if let Some(generator) = cfg.item_generator.clone() {
            item_generators.insert(cfg.axum_path.clone(), generator);
        }
    }
}

fn into_state_map(configs: Vec<RouteConfig>) -> HashMap<String, RouteConfig> {
    configs
        .into_iter()
        .map(|cfg| {
            let key = route_key(&cfg.method.as_str().to_uppercase(), &cfg.axum_path);
            (key, cfg)
        })
        .collect()
}

fn route_key(method: &str, path: &str) -> String {
    format!("{} {}", method, path)
}

async fn handle_readonly(
    State(state): State<Arc<AppState>>,
    matched: MatchedPath,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let key = route_key(method.as_str(), matched.as_str());
    let cfg = match state.routes.get(&key) {
        Some(c) => c,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let concrete = uri.path();

    match method {
        Method::GET if is_item_pattern(matched.as_str()) => get_item(&state, cfg, concrete),
        Method::GET => get_collection(&state, cfg, concrete, &uri, &headers),
        Method::DELETE => delete_item(&state, cfg, concrete),
        _ => {
            if cfg.status_code == StatusCode::NO_CONTENT.as_u16() {
                StatusCode::NO_CONTENT.into_response()
            } else {
                json_response(cfg.status_code, cfg.body.clone())
            }
        }
    }
}

async fn handle_with_body(
    State(state): State<Arc<AppState>>,
    matched: MatchedPath,
    uri: Uri,
    method: Method,
    body: Option<Json<serde_json::Value>>,
) -> Response {
    let key = route_key(method.as_str(), matched.as_str());
    let cfg = match state.routes.get(&key) {
        Some(c) => c,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let request_fields = body.map(|Json(b)| b);
    let concrete = uri.path();

    match method {
        Method::POST => post_item(&state, cfg, concrete, request_fields),
        _ => put_or_patch(&state, cfg, concrete, request_fields, &method),
    }
}

fn get_item(state: &AppState, cfg: &RouteConfig, concrete: &str) -> Response {
    let id = concrete.rsplit('/').next().unwrap_or("").to_string();
    let mut fallback = cfg.body.clone().unwrap_or(serde_json::Value::Null);
    if let Some(obj) = fallback.as_object_mut() {
        obj.insert("id".to_string(), serde_json::Value::String(id));
    }

    let mut store = state.store.write().unwrap();
    store.seed_item(concrete, fallback);
    json_response(cfg.status_code, store.get_item(concrete).cloned())
}

fn get_collection(
    state: &AppState,
    cfg: &RouteConfig,
    concrete: &str,
    uri: &Uri,
    headers: &HeaderMap,
) -> Response {
    let mut store = state.store.write().unwrap();
    if !store.collection_initialized(concrete) {
        let templates = state
            .collection_templates
            .get(&cfg.axum_path)
            .cloned()
            .unwrap_or_default();
        let filled = if !use_examples() {
            if let Some(generator) = state.item_generators.get(&cfg.axum_path) {
                let generator = Arc::clone(generator);
                fill_to_count_with_generator(state.min_items, state.max_items, move || generator())
            } else {
                fill_to_count(templates, state.min_items, state.max_items)
            }
        } else {
            fill_to_count(templates, state.min_items, state.max_items)
        };
        for item in filled {
            let id = item
                .get("id")
                .map(json_value_to_string)
                .unwrap_or_else(new_uuid);
            store.seed_item(&format!("{}/{}", concrete, id), item);
        }
        store.init_collection(concrete);
    }
    let items = store.collection_items(concrete).unwrap_or_default();
    let body = if cfg.offset_pagination {
        let mut offset = 0usize;
        let mut limit = state.max_items.max(1);
        for pair in uri.query().unwrap_or_default().split('&') {
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            if name == "offset" || name == "limit" {
                let Ok(value) = value.parse::<usize>() else {
                    return StatusCode::BAD_REQUEST.into_response();
                };
                if name == "offset" {
                    offset = value;
                } else {
                    limit = value;
                }
            }
        }
        if limit == 0 {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let total = items.len();
        let page = items.into_iter().skip(offset).take(limit).collect();
        let mut body = build_collection_response(&cfg.body, page);
        let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let link = |start| {
            serde_json::Value::String(format!(
                "http://{host}{concrete}?offset={start}&limit={limit}"
            ))
        };
        body["count"] = total.into();
        body["next"] = if offset.saturating_add(limit) < total {
            link(offset + limit)
        } else {
            serde_json::Value::Null
        };
        body["previous"] = if offset > 0 {
            link(offset.saturating_sub(limit))
        } else {
            serde_json::Value::Null
        };
        body
    } else {
        build_collection_response(&cfg.body, items)
    };
    json_response(cfg.status_code, Some(body))
}

fn delete_item(state: &AppState, cfg: &RouteConfig, concrete: &str) -> Response {
    state.store.write().unwrap().delete_item(concrete);
    if cfg.status_code == StatusCode::NO_CONTENT.as_u16() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        json_response(cfg.status_code, cfg.body.clone())
    }
}

fn post_item(
    state: &AppState,
    cfg: &RouteConfig,
    collection: &str,
    request_fields: Option<serde_json::Value>,
) -> Response {
    let location_only = cfg.body.is_none() && cfg.discriminator_field.is_none();

    let base = if let (Some(disc_field), Some(variants)) = (&cfg.discriminator_field, &cfg.variants)
    {
        let disc_value = request_fields
            .as_ref()
            .and_then(|b| b.get(disc_field))
            .and_then(|v| v.as_str());
        disc_value
            .and_then(|d| variants.get(d))
            .or_else(|| variants.values().next())
            .cloned()
    } else if location_only {
        state
            .collection_templates
            .get(collection)
            .and_then(|items| items.first())
            .cloned()
            .or_else(|| Some(serde_json::Value::Object(serde_json::Map::new())))
    } else {
        cfg.body.clone()
    };

    let mut new_item = merge_excluding(base, request_fields, &cfg.read_only_fields);

    if location_only && let Some(obj) = new_item.as_object_mut() {
        let fresh_id = obj
            .get("id")
            .and_then(new_id_like)
            .unwrap_or_else(|| serde_json::Value::String(new_uuid()));
        obj.insert("id".to_string(), fresh_id);
    }

    let id = new_item
        .get("id")
        .map(json_value_to_string)
        .unwrap_or_else(new_uuid);

    let item_path = format!("{}/{}", collection, id);

    let mut store = state.store.write().unwrap();
    store.put_item(&item_path, new_item.clone());

    if location_only {
        let status = StatusCode::from_u16(cfg.status_code).unwrap_or(StatusCode::CREATED);
        (status, [(axum::http::header::LOCATION, item_path)]).into_response()
    } else {
        json_response(cfg.status_code, Some(new_item))
    }
}

fn put_or_patch(
    state: &AppState,
    cfg: &RouteConfig,
    concrete: &str,
    request_fields: Option<serde_json::Value>,
    method: &Method,
) -> Response {
    let base = if *method == Method::PATCH {
        let store = state.store.read().unwrap();
        store
            .get_item(concrete)
            .cloned()
            .or_else(|| cfg.body.clone())
    } else {
        cfg.body.clone()
    };

    let updated = merge_excluding(base, request_fields, &cfg.read_only_fields);
    state
        .store
        .write()
        .unwrap()
        .put_item(concrete, updated.clone());
    json_response(cfg.status_code, Some(updated))
}

fn merge_excluding(
    base: Option<serde_json::Value>,
    overlay: Option<serde_json::Value>,
    protected: &std::collections::HashSet<String>,
) -> serde_json::Value {
    let mut result = base.unwrap_or(serde_json::Value::Null);
    if let (Some(obj), Some(serde_json::Value::Object(fields))) = (result.as_object_mut(), overlay)
    {
        for (k, v) in fields {
            if !protected.contains(&k) {
                obj.insert(k, v);
            }
        }
    }
    result
}

#[cfg(test)]
fn merge(base: Option<serde_json::Value>, overlay: Option<serde_json::Value>) -> serde_json::Value {
    merge_excluding(base, overlay, &Default::default())
}

fn json_response(status_code: u16, body: Option<serde_json::Value>) -> Response {
    let status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK);
    let payload = body.unwrap_or(serde_json::Value::Null);
    (status, Json(payload)).into_response()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{merge, merge_excluding};

    fn empty_state() -> std::sync::Arc<super::AppState> {
        std::sync::Arc::new(super::AppState::default())
    }

    // --- merge: read-only field protection ---

    #[test]
    fn merge_does_not_override_protected_fields_from_base() {
        let base = json!({ "id": "server-id", "name": "old" });
        let overlay = json!({ "id": "client-id", "name": "new" });
        let protected = std::collections::HashSet::from(["id".to_string()]);
        let result = merge_excluding(Some(base), Some(overlay), &protected);
        assert_eq!(
            result["id"],
            json!("server-id"),
            "readOnly field should keep server value"
        );
        assert_eq!(
            result["name"],
            json!("new"),
            "non-readOnly field should be overridden"
        );
    }

    // --- merge: basic behaviour ---

    #[test]
    fn overlay_value_wins_over_base() {
        let base = json!({ "storyPoints": 5, "type": "feature" });
        let overlay = json!({ "storyPoints": 6 });
        assert_eq!(merge(Some(base), Some(overlay))["storyPoints"], json!(6));
    }

    #[test]
    fn base_value_kept_when_not_in_overlay() {
        let base = json!({ "id": "abc", "type": "feature" });
        let overlay = json!({ "type": "feature" });
        assert_eq!(merge(Some(base), Some(overlay))["id"], json!("abc"));
    }

    #[test]
    fn unknown_overlay_field_is_included() {
        let base = json!({ "type": "feature" });
        let overlay = json!({ "type": "feature", "coreRepo": "IAM" });
        assert_eq!(merge(Some(base), Some(overlay))["coreRepo"], json!("IAM"));
    }

    #[test]
    fn no_overlay_returns_base_unchanged() {
        let base = json!({ "storyPoints": 5 });
        assert_eq!(merge(Some(base.clone()), None), base);
    }

    // --- HTTP handler integration tests ---

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use crate::http_method::HttpMethod;
    use crate::spec_parser::RouteConfig;

    use super::{build, build_with_bounds};

    fn route(method: HttpMethod, path: &str, status: u16, body: Option<Value>) -> RouteConfig {
        RouteConfig {
            axum_path: path.to_string(),
            method,
            status_code: status,
            body,
            ..Default::default()
        }
    }

    async fn send(app: axum::Router, req: Request<Body>) -> axum::response::Response {
        ServiceExt::<Request<Body>>::oneshot(app, req)
            .await
            .unwrap()
    }

    async fn response_json(response: axum::response::Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn get_returns_precomputed_body_with_200() {
        let app = build(vec![route(
            HttpMethod::Get,
            "/items",
            200,
            Some(json!({"id": "abc"})),
        )]);
        let response = send(
            app,
            Request::builder()
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_json(response).await["id"], json!("abc"));
    }

    #[tokio::test]
    async fn delete_returns_204_with_no_meaningful_body() {
        let app = build(vec![route(HttpMethod::Delete, "/items/{id}", 204, None)]);
        let response = send(
            app,
            Request::builder()
                .method(Method::DELETE)
                .uri("/items/123")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn options_route_with_204_status_returns_no_content() {
        let app = build(vec![route(HttpMethod::Options, "/items", 204, None)]);
        let response = send(
            app,
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_with_non_204_status_returns_body() {
        let app = build(vec![route(
            HttpMethod::Delete,
            "/items/{id}",
            200,
            Some(json!({"deleted": true})),
        )]);
        let response = send(
            app,
            Request::builder()
                .method(Method::DELETE)
                .uri("/items/abc")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_json(response).await["deleted"], json!(true));
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let app = build(vec![route(HttpMethod::Get, "/items", 200, Some(json!({})))]);
        let response = send(
            app,
            Request::builder()
                .uri("/missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_dispatches_to_correct_variant_by_discriminator() {
        let app = build(vec![RouteConfig {
            axum_path: "/items".to_string(),
            method: HttpMethod::Post,
            status_code: 201,
            discriminator_field: Some("kind".to_string()),
            variants: Some(
                [
                    ("a".to_string(), json!({"kind": "a", "value": 1})),
                    ("b".to_string(), json!({"kind": "b", "value": 2})),
                ]
                .into(),
            ),
            ..Default::default()
        }]);
        let response = send(
            app,
            Request::builder()
                .method(Method::POST)
                .uri("/items")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"kind":"b"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response_json(response).await;
        assert_eq!(body["kind"], json!("b"));
        assert_eq!(body["value"], json!(2));
    }

    #[tokio::test]
    async fn post_falls_back_to_first_variant_for_unknown_discriminator_value() {
        let app = build(vec![RouteConfig {
            axum_path: "/items".to_string(),
            method: HttpMethod::Post,
            status_code: 201,
            discriminator_field: Some("kind".to_string()),
            variants: Some([("a".to_string(), json!({"kind": "a", "fromVariantA": true}))].into()),
            ..Default::default()
        }]);
        let response = send(
            app,
            Request::builder()
                .method(Method::POST)
                .uri("/items")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"kind":"unknown"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response_json(response).await["fromVariantA"], json!(true));
    }

    #[tokio::test]
    async fn post_request_body_fields_override_variant() {
        let app = build(vec![RouteConfig {
            axum_path: "/items".to_string(),
            method: HttpMethod::Post,
            status_code: 201,
            discriminator_field: Some("kind".to_string()),
            variants: Some([("a".to_string(), json!({"kind": "a", "score": 5}))].into()),
            ..Default::default()
        }]);
        let response = send(
            app,
            Request::builder()
                .method(Method::POST)
                .uri("/items")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"kind":"a","score":9}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(response_json(response).await["score"], json!(9));
    }

    #[tokio::test]
    async fn patch_request_body_fields_override_base() {
        let app = build(vec![route(
            HttpMethod::Patch,
            "/items/{id}",
            200,
            Some(json!({"id": "abc", "status": "draft"})),
        )]);
        let response = send(
            app,
            Request::builder()
                .method(Method::PATCH)
                .uri("/items/abc")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"status":"active"}"#))
                .unwrap(),
        )
        .await;
        let body = response_json(response).await;
        assert_eq!(body["id"], json!("abc"));
        assert_eq!(body["status"], json!("active"));
    }

    #[tokio::test]
    async fn options_route_returns_precomputed_body() {
        let app = build(vec![route(
            HttpMethod::Options,
            "/items",
            200,
            Some(json!({"ok": true})),
        )]);
        let response = send(
            app,
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_json(response).await["ok"], json!(true));
    }

    #[tokio::test]
    async fn head_route_returns_200() {
        let app = build(vec![route(
            HttpMethod::Head,
            "/items",
            200,
            Some(json!({})),
        )]);
        let response = send(
            app,
            Request::builder()
                .method(Method::HEAD)
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn trace_route_returns_200() {
        let app = build(vec![route(
            HttpMethod::Trace,
            "/items",
            200,
            Some(json!({})),
        )]);
        let response = send(
            app,
            Request::builder()
                .method(Method::TRACE)
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_and_post_on_same_path_both_work() {
        let app = build(vec![
            route(
                HttpMethod::Get,
                "/items",
                200,
                Some(json!({"method": "get"})),
            ),
            route(HttpMethod::Post, "/items", 201, None),
        ]);

        let get_response = send(
            app.clone(),
            Request::builder()
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(get_response.status(), StatusCode::OK);
        assert_eq!(response_json(get_response).await["method"], json!("get"));

        let post_response = send(
            app,
            Request::builder()
                .method(Method::POST)
                .uri("/items")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
        assert_eq!(post_response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn readonly_handler_returns_404_when_route_not_in_state() {
        use super::handle_readonly;
        use axum::Router;
        let state = empty_state();
        let app = Router::new()
            .route("/items", axum::routing::get(handle_readonly))
            .with_state(state);
        let response = send(
            app,
            Request::builder()
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn body_handler_returns_404_when_route_not_in_state() {
        use super::handle_with_body;
        use axum::Router;
        let state = empty_state();
        let app = Router::new()
            .route("/items", axum::routing::post(handle_with_body))
            .with_state(state);
        let response = send(
            app,
            Request::builder()
                .method(Method::POST)
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // --- readOnly field protection (HTTP integration) ---

    #[tokio::test]
    async fn post_does_not_let_request_body_override_read_only_fields() {
        let app = build(vec![RouteConfig {
            axum_path: "/items".to_string(),
            method: HttpMethod::Post,
            status_code: 201,
            body: Some(json!({ "id": "server-id", "name": "" })),
            read_only_fields: std::collections::HashSet::from(["id".to_string()]),
            ..Default::default()
        }]);
        let response = send(
            app,
            Request::builder()
                .method(Method::POST)
                .uri("/items")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"id":"client-id","name":"Bob"}"#))
                .unwrap(),
        )
        .await;
        let body = response_json(response).await;
        assert_eq!(
            body["id"],
            json!("server-id"),
            "readOnly id should not be overridden by client"
        );
        assert_eq!(body["name"], json!("Bob"));
    }

    #[tokio::test]
    async fn put_does_not_let_request_body_override_read_only_fields() {
        let app = build(vec![RouteConfig {
            axum_path: "/items/{id}".to_string(),
            method: HttpMethod::Put,
            status_code: 200,
            body: Some(
                json!({ "id": "server-id", "createdAt": "2020-01-01T00:00:00Z", "name": "" }),
            ),
            read_only_fields: std::collections::HashSet::from([
                "id".to_string(),
                "createdAt".to_string(),
            ]),
            ..Default::default()
        }]);
        let response = send(
            app,
            Request::builder()
                .method(Method::PUT)
                .uri("/items/server-id")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"id":"hacked","createdAt":"1970-01-01T00:00:00Z","name":"Alice"}"#,
                ))
                .unwrap(),
        )
        .await;
        let body = response_json(response).await;
        assert_eq!(body["id"], json!("server-id"));
        assert_eq!(body["createdAt"], json!("2020-01-01T00:00:00Z"));
        assert_eq!(body["name"], json!("Alice"));
    }

    #[tokio::test]
    async fn patch_does_not_let_request_body_override_read_only_fields() {
        let app = build(vec![RouteConfig {
            axum_path: "/items/{id}".to_string(),
            method: HttpMethod::Patch,
            status_code: 200,
            body: Some(
                json!({ "id": "server-id", "createdAt": "2020-01-01T00:00:00Z", "name": "old" }),
            ),
            read_only_fields: std::collections::HashSet::from([
                "id".to_string(),
                "createdAt".to_string(),
            ]),
            ..Default::default()
        }]);
        let response = send(
            app,
            Request::builder()
                .method(Method::PATCH)
                .uri("/items/server-id")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"createdAt":"1970-01-01T00:00:00Z","name":"new"}"#,
                ))
                .unwrap(),
        )
        .await;
        let body = response_json(response).await;
        assert_eq!(body["createdAt"], json!("2020-01-01T00:00:00Z"));
        assert_eq!(body["name"], json!("new"));
    }

    // --- CRUD state tests ---

    #[tokio::test]
    async fn get_item_reflects_stored_value_from_prior_put() {
        let mock = Some(json!({"id": "m", "name": "mock"}));
        let app = build(vec![
            route(HttpMethod::Get, "/items/{id}", 200, mock.clone()),
            route(HttpMethod::Put, "/items/{id}", 200, mock),
        ]);
        send(
            app.clone(),
            Request::builder()
                .method(Method::PUT)
                .uri("/items/abc")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"updated"}"#))
                .unwrap(),
        )
        .await;
        let response = send(
            app,
            Request::builder()
                .uri("/items/abc")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response_json(response).await["name"], json!("updated"));
    }

    #[tokio::test]
    async fn get_item_id_matches_path_segment_on_first_access() {
        let app = build(vec![route(
            HttpMethod::Get,
            "/items/{id}",
            200,
            Some(json!({"id": "mock-uuid", "name": "x"})),
        )]);
        let response = send(
            app,
            Request::builder()
                .uri("/items/my-specific-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response_json(response).await["id"], json!("my-specific-id"));
    }

    #[tokio::test]
    async fn post_with_no_body_returns_201_with_location_header() {
        let app = build(vec![route(HttpMethod::Post, "/items", 201, None)]);
        let response = send(
            app,
            Request::builder()
                .method(Method::POST)
                .uri("/items")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"thing"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let location = response
            .headers()
            .get("location")
            .expect("response should contain a Location header");
        assert!(location.to_str().unwrap().starts_with("/items/"));
    }

    #[tokio::test]
    async fn post_with_no_body_is_retrievable_via_get() {
        let app = build(vec![
            route(HttpMethod::Post, "/items", 201, None),
            route(HttpMethod::Get, "/items/{id}", 200, None),
        ]);
        let post_response = send(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/items")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"thing"}"#))
                .unwrap(),
        )
        .await;
        let location = post_response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let get_response = send(
            app,
            Request::builder()
                .uri(&location)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(get_response.status(), StatusCode::OK);
        let body = response_json(get_response).await;
        assert_eq!(body["name"], json!("thing"));
        assert!(body["id"].is_string());
        assert_eq!(
            body["id"].as_str().unwrap(),
            location.rsplit('/').next().unwrap(),
            "body id should match the location path segment"
        );
        assert!(
            body.get("createdAt").is_none(),
            "invented fields should not be present"
        );
    }

    #[tokio::test]
    async fn post_item_is_retrievable_via_get_item() {
        let app = build(vec![
            route(
                HttpMethod::Post,
                "/items",
                201,
                Some(json!({"id": "mock-id", "name": ""})),
            ),
            route(
                HttpMethod::Get,
                "/items/{id}",
                200,
                Some(json!({"id": "mock-id", "name": "mock"})),
            ),
        ]);
        let post_resp = send(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/items")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"created"}"#))
                .unwrap(),
        )
        .await;
        let post_body = response_json(post_resp).await;
        let new_id = post_body["id"].as_str().unwrap().to_string();
        let get_resp = send(
            app,
            Request::builder()
                .uri(format!("/items/{}", new_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response_json(get_resp).await["name"], json!("created"));
    }

    #[tokio::test]
    async fn post_item_appears_in_get_collection() {
        let app = build(vec![
            route(
                HttpMethod::Post,
                "/items",
                201,
                Some(json!({"id": "new-id", "name": ""})),
            ),
            route(
                HttpMethod::Get,
                "/items",
                200,
                Some(json!({"total": 0, "items": []})),
            ),
        ]);
        send(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/items")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"new"}"#))
                .unwrap(),
        )
        .await;
        let response = send(
            app,
            Request::builder()
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let body = response_json(response).await;
        let items = body["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["name"], json!("new"));
    }

    #[tokio::test]
    async fn patch_preserves_fields_not_in_request_body_from_prior_put() {
        let mock = Some(json!({"id": "m", "name": "mock", "color": "mock"}));
        let app = build(vec![
            route(HttpMethod::Get, "/items/{id}", 200, mock.clone()),
            route(HttpMethod::Put, "/items/{id}", 200, mock.clone()),
            route(HttpMethod::Patch, "/items/{id}", 200, mock),
        ]);
        send(
            app.clone(),
            Request::builder()
                .method(Method::PUT)
                .uri("/items/abc")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"from-put","color":"red"}"#))
                .unwrap(),
        )
        .await;
        send(
            app.clone(),
            Request::builder()
                .method(Method::PATCH)
                .uri("/items/abc")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"color":"blue"}"#))
                .unwrap(),
        )
        .await;
        let response = send(
            app,
            Request::builder()
                .uri("/items/abc")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let body = response_json(response).await;
        assert_eq!(body["name"], json!("from-put"));
        assert_eq!(body["color"], json!("blue"));
    }

    #[tokio::test]
    async fn put_request_body_fields_override_base() {
        let app = build(vec![route(
            HttpMethod::Put,
            "/items/{id}",
            200,
            Some(json!({"id": "abc", "name": "old"})),
        )]);
        let response = send(
            app,
            Request::builder()
                .method(Method::PUT)
                .uri("/items/abc")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"new"}"#))
                .unwrap(),
        )
        .await;
        let body = response_json(response).await;
        assert_eq!(body["id"], json!("abc"));
        assert_eq!(body["name"], json!("new"));
    }

    // --- multi-item collection tests ---

    #[tokio::test]
    async fn get_collection_default_item_count_is_between_one_and_twenty_four() {
        let app = build(vec![route(
            HttpMethod::Get,
            "/items",
            200,
            Some(json!([{"id": "seed", "name": "x"}])),
        )]);
        let response = send(
            app,
            Request::builder()
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let count = response_json(response).await.as_array().unwrap().len();
        assert!(
            (1..=24).contains(&count),
            "expected 1..=24 items, got {count}"
        );
    }

    #[tokio::test]
    async fn get_collection_item_count_is_within_explicitly_configured_bounds() {
        let app = build_with_bounds(
            vec![route(
                HttpMethod::Get,
                "/items",
                200,
                Some(json!([{"id": "seed", "name": "x"}])),
            )],
            5,
            5,
        );
        let response = send(
            app,
            Request::builder()
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response_json(response).await.as_array().unwrap().len(), 5);
    }

    // --- use_examples collection generation ---

    struct UseExamplesGuard;
    impl Drop for UseExamplesGuard {
        fn drop(&mut self) {
            crate::resource_generator::set_use_examples(DEFAULT_USE_EXAMPLES);
        }
    }

    #[tokio::test]
    async fn get_collection_with_use_examples_false_uses_item_generator_when_present() {
        use std::sync::Arc;
        crate::resource_generator::set_use_examples(false);
        let _guard = UseExamplesGuard;

        let generator: Arc<dyn Fn() -> Value + Send + Sync> =
            Arc::new(|| json!({"name": "generated"}));
        let app = build_with_bounds(
            vec![RouteConfig {
                axum_path: "/items".to_string(),
                method: HttpMethod::Get,
                status_code: 200,
                body: Some(json!([])),
                item_generator: Some(generator),
                ..Default::default()
            }],
            1,
            1,
        );
        let response = send(
            app,
            Request::builder()
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let items = body.as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["name"], json!("generated"));
    }

    // --- collection_templates filtering ---

    #[tokio::test]
    async fn collection_templates_are_not_populated_from_non_get_routes() {
        let app = build_with_bounds(
            vec![
                route(HttpMethod::Get, "/items", 200, None),
                route(
                    HttpMethod::Post,
                    "/items",
                    201,
                    Some(json!([{"id": "from-post"}])),
                ),
            ],
            1,
            1,
        );
        let response = send(
            app,
            Request::builder()
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let count = response_json(response).await.as_array().unwrap().len();
        assert_eq!(
            count, 0,
            "collection should not be seeded from non-GET route templates"
        );
    }

    #[tokio::test]
    async fn collection_templates_are_not_populated_from_get_item_routes() {
        let app = build_with_bounds(
            vec![
                route(HttpMethod::Get, "/items", 200, None),
                route(
                    HttpMethod::Get,
                    "/items/{id}",
                    200,
                    Some(json!({"id": "x", "name": "item"})),
                ),
            ],
            1,
            1,
        );
        let response = send(
            app,
            Request::builder()
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let count = response_json(response).await.as_array().unwrap().len();
        assert_eq!(
            count, 0,
            "item-pattern GET route should not seed collection templates"
        );
    }

    // --- DELETE removes item from store ---

    #[tokio::test]
    async fn delete_removes_posted_item_from_collection() {
        let app = build(vec![
            route(HttpMethod::Get, "/items", 200, Some(json!([]))),
            route(HttpMethod::Post, "/items", 201, None),
            route(HttpMethod::Delete, "/items/{id}", 204, None),
        ]);

        let post_resp = send(
            app.clone(),
            Request::builder()
                .method(Method::POST)
                .uri("/items")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"thing"}"#))
                .unwrap(),
        )
        .await;
        let location = post_resp
            .headers()
            .get("location")
            .expect("POST should return a Location header")
            .to_str()
            .unwrap()
            .to_string();

        send(
            app.clone(),
            Request::builder()
                .method(Method::DELETE)
                .uri(&location)
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        let response = send(
            app,
            Request::builder()
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let items = response_json(response).await.as_array().unwrap().len();
        assert_eq!(items, 0, "deleted item should not appear in collection");
    }

    #[tokio::test]
    async fn get_collection_with_use_examples_false_falls_back_to_templates_without_generator() {
        crate::resource_generator::set_use_examples(false);
        let _guard = UseExamplesGuard;

        let app = build_with_bounds(
            vec![route(
                HttpMethod::Get,
                "/items",
                200,
                Some(json!([{"id": "seed", "name": "x"}])),
            )],
            2,
            2,
        );
        let response = send(
            app,
            Request::builder()
                .uri("/items")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_json(response).await.as_array().unwrap().len(), 2);
    }

    // --- CORS ---

    use super::with_cors;
    use crate::cli::CorsOrigins;
    use crate::constants::DEFAULT_USE_EXAMPLES;

    #[tokio::test]
    async fn cors_wildcard_allows_any_origin() {
        let app = with_cors(
            build(vec![route(HttpMethod::Get, "/items", 200, Some(json!({})))]),
            &CorsOrigins::All,
        );
        let response = send(
            app,
            Request::builder()
                .uri("/items")
                .header("origin", "http://whatever.test")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .map(|v| v.to_str().unwrap()),
            Some("*"),
            "access-control-allow-origin should be * for CorsOrigins::All"
        );
    }

    #[tokio::test]
    async fn cors_list_reflects_matching_origin() {
        let origin = "http://localhost:3000";
        let app = with_cors(
            build(vec![route(HttpMethod::Get, "/items", 200, Some(json!({})))]),
            &CorsOrigins::List(vec![origin.to_string()]),
        );
        let response = send(
            app,
            Request::builder()
                .uri("/items")
                .header("origin", origin)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .map(|v| v.to_str().unwrap()),
            Some(origin),
            "access-control-allow-origin should reflect the matched origin"
        );
    }

    #[tokio::test]
    async fn cors_preflight_with_wildcard_returns_200() {
        let app = with_cors(
            build(vec![route(HttpMethod::Get, "/items", 200, Some(json!({})))]),
            &CorsOrigins::All,
        );
        let response = send(
            app,
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/items")
                .header("origin", "http://whatever.test")
                .header("access-control-request-method", "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "CORS preflight should return 200"
        );
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .map(|v| v.to_str().unwrap()),
            Some("*"),
            "CORS preflight should return access-control-allow-origin: *"
        );
    }
}

#[cfg(test)]
mod spec_response_tests {
    use super::build_spec_responses;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn spec_responses_do_not_invent_request_fields_or_identity() {
        let spec = yaml_serde::from_str(
            r#"
openapi: 3.0.3
info: {title: Contract, version: '1'}
paths:
  /items/{item_id}:
    get:
      responses:
        '200':
          description: detail
          content:
            application/json:
              schema:
                type: object
                properties:
                  title: {type: string}
  /items:
    post:
      responses:
        '201':
          description: acknowledgment
          content:
            application/json:
              schema:
                type: object
                properties:
                  file_path: {type: string}
"#,
        )
        .unwrap();
        let routes = crate::spec_parser::extract_routes(&spec);
        let expected: Vec<_> = routes
            .iter()
            .map(|r| (r.method.as_str().to_owned(), r.body.clone()))
            .collect();
        let app = build_spec_responses(routes);
        for (method, uri, payload) in [
            ("GET", "/items/request-id", ""),
            ("POST", "/items", r#"{"invented":true,"id":"request-id"}"#),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(payload))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(response.status().is_success());
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                Some(&body),
                expected
                    .iter()
                    .find(|(m, _)| m.eq_ignore_ascii_case(method))
                    .unwrap()
                    .1
                    .as_ref()
            );
            assert!(body.get("id").is_none());
            assert!(body.get("invented").is_none());
        }
    }
}

#[cfg(test)]
mod offset_pagination_tests {
    use super::build_with_bounds;
    use crate::{http_method::HttpMethod, spec_parser::RouteConfig};
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    #[tokio::test]
    async fn offset_pages_partition_collection_and_terminate() {
        let app = build_with_bounds(
            vec![RouteConfig {
                axum_path: "/items".into(),
                method: HttpMethod::Get,
                status_code: 200,
                body: Some(
                    json!({"count":0,"next":null,"previous":null,"results":[{"id":"seed"}]}),
                ),
                offset_pagination: true,
                ..Default::default()
            }],
            5,
            5,
        );
        let mut ids = std::collections::BTreeSet::new();
        for (offset, size, has_next) in [(0, 2, true), (2, 2, true), (4, 1, false), (6, 0, false)] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/items?offset={offset}&limit=2"))
                        .header("host", "example.test")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(response.status().is_success());
            let body: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(body["count"], 5);
            assert_eq!(body["results"].as_array().unwrap().len(), size);
            assert_eq!(!body["next"].is_null(), has_next);
            if has_next {
                assert_eq!(
                    body["next"],
                    format!("http://example.test/items?offset={}&limit=2", offset + 2)
                );
            }
            for row in body["results"].as_array().unwrap() {
                assert!(ids.insert(row["id"].as_str().unwrap().to_owned()));
            }
        }
        assert_eq!(ids.len(), 5);
        for query in ["limit=0", "offset=bad", "limit=bad"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/items?{query}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 400);
        }
    }
}
