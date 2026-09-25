//! Plasm policy over the pinned upstream Monty pool. Transport and process supervision are upstream-owned.
use monty_pool::{on_print_sync, Checkout, Pool, PoolConfig, ReplConfig, TurnEvent};
use monty_types::{MontyObject, MontyUuid, ResourceLimits};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use tokio::sync::OnceCell;

pub type Records = Vec<BTreeMap<String, String>>;
pub type TypedRecords = Vec<BTreeMap<String, serde_json::Value>>;
const MAX_OUTPUT: usize = 1_048_576;
#[derive(Default)]
pub struct PythonPool {
    binary: Option<PathBuf>,
    pool: OnceCell<Pool>,
}
impl PythonPool {
    /// The executable is a trusted deployment artifact from the same revision as monty-pool.
    pub fn with_binary(binary: PathBuf) -> Self {
        Self {
            binary: Some(binary),
            pool: OnceCell::new(),
        }
    }
    async fn get(&self) -> Result<&Pool, String> {
        self.pool
            .get_or_try_init(|| async {
                let binary = match &self.binary {
                    Some(path) => path.clone(),
                    None => match std::env::var_os("PLASM_MONTY_BINARY") {
                        Some(path) => PathBuf::from(path),
                        None => std::env::current_exe()
                            .map_err(|e| e.to_string())?
                            .with_file_name("monty"),
                    },
                };
                if !binary.is_absolute() {
                    return Err("PLASM_MONTY_BINARY must be an absolute path".into());
                }
                Pool::new(pool_config(binary))
                    .await
                    .map_err(|e| format!("Monty pool initialization: {e}"))
            })
            .await
    }
    pub(crate) async fn checkout(&self) -> Result<Checkout, String> {
        self.get()
            .await?
            .checkout(&repl_config())
            .await
            .map_err(|e| format!("Monty checkout: {e}"))
    }
    #[cfg(test)]
    pub(crate) async fn compute(&self, source: String, rows: Records) -> Result<String, String> {
        self.compute_typed(
            source,
            rows.into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|(k, v)| (k, serde_json::Value::String(v)))
                        .collect()
                })
                .collect(),
            &BTreeMap::new(),
        )
        .await
    }
    pub(crate) async fn compute_typed(
        &self,
        source: String,
        rows: TypedRecords,
        types: &BTreeMap<String, plasm_core::value_contract::ValueContract>,
    ) -> Result<String, String> {
        let mut session = self.checkout().await?;
        let class = MontyObject::class_type("Value", fresh_uuid(), true, false, []);
        let input = MontyObject::list(rows.into_iter().map(|row| {
            MontyObject::class_instance(
                class.clone(),
                fresh_uuid(),
                row.into_iter().map(|(key, value)| {
                    (
                        MontyObject::string(&key),
                        value_object(value, types.get(&key)),
                    )
                }),
            )
        }));
        let event = session
            .feed(
                &source,
                vec![("__input".into(), input)],
                vec![],
                true,
                &mut on_print_sync(|_, _| {}),
            )
            .await
            .map_err(|e| format!("Python compute: {e}"))?;
        let TurnEvent::Complete(output) = event else {
            return Err("Pure Python compute attempted an undeclared host interaction".into());
        };
        let text = output
            .as_ref()
            .as_str()
            .ok_or("Python output must be a string")?;
        if text.len() > MAX_OUTPUT {
            return Err("Python output byte budget exceeded".into());
        }
        let result = text.to_owned();
        // Only successful, validated executions return a reset worker to the pool.
        // Every error/cancellation drops the checkout and upstream kills its worker.
        session
            .finish()
            .await
            .map_err(|e| format!("Monty checkout finish: {e}"))?;
        Ok(result)
    }
    pub async fn close(&self) {
        if let Some(pool) = self.pool.get() {
            pool.close().await;
        }
    }
}
pub(crate) fn fresh_uuid() -> MontyUuid {
    MontyUuid::from_bytes(*uuid::Uuid::new_v4().as_bytes())
}
fn pool_config(binary: PathBuf) -> PoolConfig {
    let mut config = PoolConfig::subprocess(binary);
    config.min_processes = 0;
    config.max_processes = 4;
    config.checkout_timeout = Some(Duration::from_secs(5));
    config.request_timeout = Some(Duration::from_secs(3));
    config.feed_duration_limit_grace = Some(Duration::from_millis(250));
    config.turn_duration_limit_grace = Some(Duration::from_millis(250));
    config.max_checkouts_per_worker = Some(64);
    config
}
fn repl_config() -> ReplConfig {
    ReplConfig {
        script_name: "plasm_compute.py".into(),
        limits: Some(ResourceLimits {
            max_memory: Some(16 * 1024 * 1024),
            max_feed_duration: Some(Duration::from_millis(100)),
            max_turn_duration: Some(Duration::from_millis(100)),
            max_recursion_depth: 32,
            max_suspensions: 1,
            ..Default::default()
        }),
        ..Default::default()
    }
}
#[cfg(test)]
mod tests;

fn value_object(
    value: serde_json::Value,
    contract: Option<&plasm_core::value_contract::ValueContract>,
) -> MontyObject {
    use plasm_core::value_contract::ValueShape;
    if let (
        Some(plasm_core::value_contract::ValueContract {
            shape: ValueShape::Record { fields },
            ..
        }),
        serde_json::Value::Object(values),
    ) = (contract, &value)
    {
        return MontyObject::class_instance(
            MontyObject::class_type("Record", fresh_uuid(), true, false, []),
            fresh_uuid(),
            values.iter().map(|(k, v)| {
                (
                    MontyObject::string(k),
                    value_object(v.clone(), fields.get(k)),
                )
            }),
        );
    }

    match value {
        serde_json::Value::String(s) => MontyObject::string(&s),
        serde_json::Value::Bool(b) => MontyObject::bool(b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(n) => MontyObject::int(n),
            None => match n.as_u64() {
                Some(n) => MontyObject::bigint(n.into()),
                None => MontyObject::float(n.as_f64().expect("JSON number")),
            },
        },
        serde_json::Value::Null => MontyObject::none(),
        serde_json::Value::Array(values) => MontyObject::list(values.into_iter().map(|v| {
            value_object(
                v,
                contract.and_then(|t| match &t.shape {
                    ValueShape::Array { element } => Some(element.as_ref()),
                    _ => None,
                }),
            )
        })),
        serde_json::Value::Object(values) => MontyObject::dict(
            values
                .into_iter()
                .map(|(k, v)| (MontyObject::string(&k), value_object(v, None))),
        ),
    }
}
