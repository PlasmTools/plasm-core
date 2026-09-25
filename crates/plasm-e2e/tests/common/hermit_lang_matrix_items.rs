//! Deterministic stateful item routes for differential frontend tests.
//! Seed directly from the shared OpenAPI examples; preserve IDs and nested values.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
#[derive(Clone)]
struct Items(Arc<Mutex<BTreeMap<String, Value>>>);
pub(super) fn router(spec_path: &std::path::Path) -> Router {
    let spec: Value = serde_yaml::from_str(&std::fs::read_to_string(spec_path).unwrap()).unwrap();
    let list = &spec["paths"]["/language/v1/items"]["get"]["responses"]["200"]["content"]
        ["application/json"]["example"];
    let mut items: BTreeMap<String, Value> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row["id"].as_str().unwrap().into(), row.clone()))
        .collect();
    let detail = spec["paths"]["/language/v1/items/{id}"]["get"]["responses"]["200"]["content"]
        ["application/json"]["example"]
        .clone();
    items.insert("i1".into(), detail);
    let example = |path: &str, method: &str, status: &str| {
        spec["paths"][path][method]["responses"][status]["content"]["application/json"]["example"]
            .clone()
    };
    let offers = example("/language/v1/offers", "get", "200");
    let offer = example("/language/v1/offers", "post", "201");
    let notes = example("/language/v1/secured_notes", "get", "200");
    let note = example("/language/v1/secured_notes/{note_id}", "get", "200");
    let groups = example("/language/v1/secured_groups", "get", "200");
    let broadcast = example("/language/v1/items/broadcast", "post", "200");
    Router::new()
        .route(
            "/language/v1/offers",
            get(move || {
                let value = offers.clone();
                async move { Json(value) }
            })
            .post(move |Json(body): Json<Value>| {
                let mut value = offer.clone();
                async move {
                    value
                        .as_object_mut()
                        .unwrap()
                        .extend(body.as_object().unwrap().clone());
                    Json(value)
                }
            }),
        )
        .route(
            "/language/v1/secured_notes",
            get(move || {
                let value = notes.clone();
                async move { Json(value) }
            }),
        )
        .route(
            "/language/v1/secured_notes/{note_id}",
            get(move || {
                let value = note.clone();
                async move { Json(value) }
            }),
        )
        .route(
            "/language/v1/secured_groups",
            get(move || {
                let value = groups.clone();
                async move { Json(value) }
            }),
        )
        .route(
            "/language/v1/items/broadcast",
            post(move || {
                let value = broadcast.clone();
                async move { Json(value) }
            }),
        )
        .route(
            "/language/v1/items/{id}/secured_touch",
            post(|Path(id): Path<String>| async move { Json(json!({"ok": true, "id": id})) }),
        )
        .route("/language/v1/items", get(list_items).post(create))
        .route("/language/v1/items/search", get(search))
        .route(
            "/language/v1/items/{id}",
            get(one).patch(update).delete(remove),
        )
        .with_state(Items(Arc::new(Mutex::new(items))))
}
async fn list_items(
    State(items): State<Items>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Json<Value> {
    let rows: Vec<_> = items
        .0
        .lock()
        .unwrap()
        .values()
        .filter(|row| {
            query
                .get("owner")
                .is_none_or(|owner| row["owner"].as_str() == Some(owner))
        })
        .cloned()
        .collect();
    let offset = query
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let limit = query
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(rows.len());
    Json(Value::Array(
        rows.into_iter().skip(offset).take(limit).collect(),
    ))
}
async fn search(
    State(items): State<Items>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Json<Value> {
    let text = query.get("q").map(String::as_str).unwrap_or("");
    Json(Value::Array(
        items
            .0
            .lock()
            .unwrap()
            .values()
            .filter(|row| {
                row["title"]
                    .as_str()
                    .is_some_and(|title| title.contains(text))
            })
            .cloned()
            .collect(),
    ))
}
async fn one(State(items): State<Items>, Path(id): Path<String>) -> Json<Value> {
    let items = items.0.lock().unwrap();
    let mut row = items
        .get(&id)
        .cloned()
        .unwrap_or_else(|| items["i1"].clone());
    row["id"] = json!(id);
    Json(row)
}
async fn create(State(items): State<Items>, Json(mut row): Json<Value>) -> Json<Value> {
    let mut items = items.0.lock().unwrap();
    let id = format!("created-{}", items.len());
    row["id"] = json!(id);
    items.insert(id, row.clone());
    Json(row)
}
async fn update(
    State(items): State<Items>,
    Path(id): Path<String>,
    Json(patch): Json<Value>,
) -> Json<Value> {
    let mut items = items.0.lock().unwrap();
    let row = items.entry(id.clone()).or_insert_with(|| json!({"id": id}));
    row.as_object_mut()
        .unwrap()
        .extend(patch.as_object().unwrap().clone());
    Json(row.clone())
}
async fn remove(State(items): State<Items>, Path(id): Path<String>) -> StatusCode {
    items.0.lock().unwrap().remove(&id);
    StatusCode::NO_CONTENT
}
