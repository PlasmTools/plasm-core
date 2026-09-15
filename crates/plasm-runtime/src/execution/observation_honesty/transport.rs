//! Scripted HTTP transport backed by the reference model's backend + barrier schedule.

use super::ref_model::{BackendState, ReferenceModel, ResponseFault, ResponseRecord};
use super::schedule::BarrierSchedule;
use crate::auth::ResolvedAuth;
use crate::error::RuntimeError;
use crate::http_transport::HttpTransport;
use async_trait::async_trait;
use plasm_compile::{CompiledRequest, HttpMethod};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Create outcome scripted by attempt index.
#[derive(Debug, Clone)]
pub enum CreateScript {
    /// Succeed and commit to backend.
    Ok,
    /// Fail with HTTP status (no backend commit).
    Fail { status: u16 },
}

/// Shared mutable ledger for property runs.
#[derive(Clone)]
pub struct HonestyHarness {
    pub model: Arc<Mutex<ReferenceModel>>,
    /// Per create attempt (0-based) → outcome. Extra attempts beyond len succeed.
    pub create_script: Arc<Mutex<Vec<CreateScript>>>,
    pub create_attempts: Arc<Mutex<usize>>,
    /// Optional barrier for GET hydrate waves (None = immediate).
    pub schedule: Option<Arc<BarrierSchedule>>,
    /// Optional per-request fault map: requested note_id → fault kind.
    pub note_faults: Arc<Mutex<BTreeMap<String, ResponseFault>>>,
    /// When WrongFields: field values to inject (title/body).
    pub injected_fields: Arc<Mutex<BTreeMap<String, BTreeMap<String, String>>>>,
}

impl HonestyHarness {
    pub fn new() -> Self {
        Self {
            model: Arc::new(Mutex::new(ReferenceModel::default())),
            create_script: Arc::new(Mutex::new(Vec::new())),
            create_attempts: Arc::new(Mutex::new(0)),
            schedule: None,
            note_faults: Arc::new(Mutex::new(BTreeMap::new())),
            injected_fields: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn with_schedule(mut self, schedule: Arc<BarrierSchedule>) -> Self {
        self.schedule = Some(schedule);
        self
    }

    pub fn set_create_script(&self, script: Vec<CreateScript>) {
        *self.create_script.lock().expect("script") = script;
        *self.create_attempts.lock().expect("attempts") = 0;
    }

    pub fn backend_mut<R>(&self, f: impl FnOnce(&mut BackendState) -> R) -> R {
        let mut model = self.model.lock().expect("model");
        f(&mut model.backend)
    }

    pub fn with_model<R>(&self, f: impl FnOnce(&ReferenceModel) -> R) -> R {
        let model = self.model.lock().expect("model");
        f(&model)
    }

    pub fn with_model_mut<R>(&self, f: impl FnOnce(&mut ReferenceModel) -> R) -> R {
        let mut model = self.model.lock().expect("model");
        f(&mut model)
    }
}

#[async_trait]
impl HttpTransport for HonestyHarness {
    async fn send_compiled_http(
        &self,
        _base_url: &str,
        request: &CompiledRequest,
        _auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        // --- Partial-write expense creates ---
        if matches!(request.method, HttpMethod::Post) && request.path.ends_with("/expenses") {
            return self.handle_expense_create(request).await;
        }

        // --- Expense list / relation ---
        if matches!(request.method, HttpMethod::Get) && request.path.contains("/expenses") {
            return self.handle_expense_list(request);
        }

        // --- Group get ---
        if matches!(request.method, HttpMethod::Get)
            && request.path.contains("/groups/")
            && !request.path.contains("/expenses")
        {
            let id = request.path.rsplit('/').next().unwrap_or("g1");
            let name = self.with_model(|m| {
                m.backend
                    .groups
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| "Trip".into())
            });
            return Ok((json!({"group_id": id, "name": name}), None));
        }

        // --- Secured-note hydrate GETs ---
        if matches!(request.method, HttpMethod::Get) && request.path.contains("/secured_notes/") {
            return self.handle_note_get(request).await;
        }

        Err(RuntimeError::CacheError {
            message: format!("unexpected request {:?} {}", request.method, request.path),
        })
    }

    async fn get_json_absolute(
        &self,
        _url: &str,
        _auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        Ok((json!({}), None))
    }
}

impl HonestyHarness {
    async fn handle_expense_create(
        &self,
        request: &CompiledRequest,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        use plasm_core::Value;
        let body = request
            .body
            .clone()
            .unwrap_or(Value::Object(Default::default()));
        let (group_id, description) = match &body {
            Value::Object(m) => (
                match m.get("group_id") {
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                },
                match m.get("description") {
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                },
            ),
            _ => (String::new(), String::new()),
        };

        let attempt = {
            let mut n = self.create_attempts.lock().expect("attempts");
            let i = *n;
            *n += 1;
            i
        };
        let outcome = {
            let script = self.create_script.lock().expect("script");
            script.get(attempt).cloned().unwrap_or(CreateScript::Ok)
        };

        match outcome {
            CreateScript::Fail { status } => Err(RuntimeError::RequestError {
                message: format!(
                    "HTTP request failed: POST {} — HTTP {status} from API: harness reject",
                    request.path
                ),
                attempts: 1,
                status: Some(status),
                body: None,
            }),
            CreateScript::Ok => {
                let expense = self.with_model_mut(|m| {
                    m.backend.ensure_group(&group_id, "Trip");
                    let expense = m.backend.commit_expense(&group_id, &description);
                    m.record_commit(&expense);
                    expense
                });
                Ok((
                    json!({
                        "expense_id": expense.expense_id,
                        "group_id": expense.group_id,
                        "description": expense.description,
                    }),
                    None,
                ))
            }
        }
    }

    fn handle_expense_list(
        &self,
        request: &CompiledRequest,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let parts: Vec<&str> = request.path.trim_matches('/').split('/').collect();
        let group_id = parts
            .iter()
            .position(|p| *p == "groups")
            .and_then(|i| parts.get(i + 1))
            .copied()
            .unwrap_or("");
        let rows: Vec<serde_json::Value> = self.with_model(|m| {
            m.backend
                .expenses
                .values()
                .filter(|e| e.group_id == group_id)
                .map(|e| {
                    json!({
                        "expense_id": e.expense_id,
                        "group_id": e.group_id,
                        "description": e.description,
                    })
                })
                .collect()
        });
        Ok((json!(rows), None))
    }

    async fn handle_note_get(
        &self,
        request: &CompiledRequest,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let requested = request
            .path
            .rsplit('/')
            .next()
            .expect("secured_notes path")
            .to_string();

        if let Some(sched) = &self.schedule {
            sched.park(requested.clone()).await;
        }

        let fault = self
            .note_faults
            .lock()
            .expect("faults")
            .get(&requested)
            .copied()
            .unwrap_or(ResponseFault::None);

        let (body_id, title, body) = match fault {
            ResponseFault::None => {
                let note = self.with_model(|m| m.backend.notes.get(&requested).cloned());
                let Some(note) = note else {
                    return Err(RuntimeError::CacheError {
                        message: format!("harness: unknown note {requested}"),
                    });
                };
                (note.note_id, note.title, note.body)
            }
            ResponseFault::WrongIdentity => {
                let injected = self
                    .injected_fields
                    .lock()
                    .expect("inject")
                    .get(&requested)
                    .cloned()
                    .unwrap_or_default();
                (
                    injected
                        .get("note_id")
                        .cloned()
                        .unwrap_or_else(|| "injected-id".into()),
                    injected
                        .get("title")
                        .cloned()
                        .unwrap_or_else(|| "injected-title".into()),
                    injected
                        .get("body")
                        .cloned()
                        .unwrap_or_else(|| "injected-body".into()),
                )
            }
            ResponseFault::WrongFields => {
                let injected = self
                    .injected_fields
                    .lock()
                    .expect("inject")
                    .get(&requested)
                    .cloned()
                    .unwrap_or_default();
                // Identity correct; fields lie.
                (
                    requested.clone(),
                    injected
                        .get("title")
                        .cloned()
                        .unwrap_or_else(|| "lie-title".into()),
                    injected
                        .get("body")
                        .cloned()
                        .unwrap_or_else(|| "lie-body".into()),
                )
            }
        };

        let mut fields = BTreeMap::new();
        fields.insert("title".into(), title.clone());
        fields.insert("body".into(), body.clone());

        self.with_model_mut(|m| {
            m.responses.push(ResponseRecord {
                request_id: format!("get:{requested}"),
                requested_identity: requested.clone(),
                body_identity: body_id.clone(),
                fields: fields.clone(),
                fault,
            });
        });

        if let Some(schedule) = &self.schedule {
            schedule.complete(&requested);
        }

        Ok((
            json!({
                "note_id": body_id,
                "title": title,
                "body": body,
            }),
            None,
        ))
    }
}
