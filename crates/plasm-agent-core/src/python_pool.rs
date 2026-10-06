//! Plasm policy over the pinned upstream Monty pool. Transport and process supervision are upstream-owned.
mod temporal_codec;
use monty_pool::{on_print_sync, Checkout, Pool, PoolConfig, ReplConfig, TurnEvent};
use monty_types::{MontyObject, MontyUuid, ResourceLimits};
use plasm_runtime::{ExecutionFailure, FailureCause};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use thiserror::Error;
use tokio::sync::OnceCell;

pub type Records = Vec<BTreeMap<String, String>>;
pub type TypedRecords = Vec<BTreeMap<String, plasm_core::Value>>;

#[derive(Debug, Error)]
enum PythonValueConversionError {
    #[error(transparent)]
    Temporal(#[from] plasm_core::temporal_value::TemporalValueError),
    #[error("Python union value has no matching representation")]
    UnionRepresentationMissing,
    #[error("Python record contains an undeclared field")]
    UndeclaredRecordField,
    #[error("mapping input requires a record contract")]
    MappingRequiresRecord,
    #[error("mapping input requires an object")]
    MappingRequiresObject,
    #[error("mapping input contains an undeclared field")]
    UndeclaredMappingField,
    #[error("set input requires an array")]
    SetRequiresArray,
    #[error("Python input contains an unresolved or non-finite value")]
    UnsupportedValue,
    #[error("temporal component `{name}` is absent after component validation")]
    MissingTemporalComponent { name: &'static str },
}

#[derive(Debug, Error)]
enum PythonOutputError {
    #[error("Python output exceeded the depth or size budget")]
    BudgetExceeded,
    #[error("Python integer representation is invalid")]
    InvalidInteger,
    #[error("Python integer exceeds the Plasm integer range")]
    IntegerOutOfRange,
    #[error("Python float representation is invalid")]
    InvalidFloat,
    #[error("Python output must be finite")]
    NonFiniteFloat,
    #[error("Python collection representation is invalid")]
    InvalidCollection,
    #[error("Python output is not a Plasm value")]
    UnsupportedValue,
    #[error("Python record keys must be strings")]
    NonStringRecordKey,
    #[error("Python record contains a duplicate field")]
    DuplicateRecordField,
    #[error("Python output byte budget exceeded")]
    ByteBudgetExceeded,
    #[error(transparent)]
    Temporal(#[from] PythonValueConversionError),
}

#[derive(Debug, Error)]
enum PythonMoneyCallError {
    #[error("{function}: unexpected arguments")]
    UnexpectedArguments { function: String },
    #[error("{function}: duplicate argument `{name}`")]
    DuplicateArgument {
        function: String,
        name: &'static str,
    },
    #[error("{function}: missing argument `{name}`")]
    MissingArgument {
        function: String,
        name: &'static str,
    },
    #[error(transparent)]
    Output(#[from] PythonOutputError),
    #[error(transparent)]
    Money(#[from] crate::python_money::PythonMoneyError),
}

#[derive(Debug, Error)]
pub(crate) enum PythonReturnValueError {
    #[error("Python output must be a string")]
    ExpectedString,
    #[error(transparent)]
    Money(#[from] crate::python_money::PythonMoneyError),
    #[error(transparent)]
    Temporal(#[from] plasm_core::temporal_value::TemporalValueError),
    #[error("money catalog `{entry_id}` is absent")]
    MoneyCatalogMissing { entry_id: String },
    #[error("expected array output")]
    ExpectedArray,
    #[error("expected dictionary output")]
    ExpectedDictionary,
    #[error("expected record output")]
    ExpectedRecord,
    #[error("output field `{field}` is not declared")]
    UnknownRecordField { field: String },
    #[error("output does not match any declared union variant")]
    UnmatchedUnion,
    #[error(transparent)]
    Contract(#[from] plasm_core::value_contract::ValueContractError),
}

const MAX_OUTPUT: usize = 1_048_576;
#[derive(Default)]
pub struct PythonPool {
    binary: Option<PathBuf>,
    clock: Option<monty_types::DateTimeSource>,
    pool: OnceCell<Pool>,
}
impl PythonPool {
    /// The executable is a trusted deployment artifact from the same revision as monty-pool.
    pub fn with_binary(binary: PathBuf) -> Self {
        Self {
            binary: Some(binary),
            clock: None,
            pool: OnceCell::new(),
        }
    }
    /// Evaluation clock injection is scoped to this pool, never global mutable state.
    pub fn with_clock(mut self, clock: monty_types::DateTimeSource) -> Self {
        self.clock = Some(clock);
        self
    }
    async fn get(&self) -> Result<&Pool, ExecutionFailure> {
        self.pool
            .get_or_try_init(|| async {
                let binary = match &self.binary {
                    Some(path) => path.clone(),
                    None => match std::env::var_os("PLASM_MONTY_BINARY") {
                        Some(path) => PathBuf::from(path),
                        None => std::env::current_exe()
                            .map_err(|e| infrastructure_failure(e.to_string()))?
                            .with_file_name("monty"),
                    },
                };
                if !binary.is_absolute() {
                    return Err(infrastructure_failure(
                        "PLASM_MONTY_BINARY must be an absolute path",
                    ));
                }
                Pool::new(pool_config(binary)).await.map_err(pool_failure)
            })
            .await
    }
    pub(crate) async fn checkout(&self) -> Result<Checkout, ExecutionFailure> {
        self.checkout_with_suspensions(1).await
    }
    async fn checkout_with_suspensions(
        &self,
        max_suspensions: usize,
    ) -> Result<Checkout, ExecutionFailure> {
        let mut config = repl_config();
        config
            .limits
            .as_mut()
            .expect("configured limits")
            .max_suspensions = max_suspensions;
        if let Some(clock) = self.clock {
            config.os_policy.datetime = clock;
        } else if std::env::var("PLASM_TEMPORAL_NOW").is_ok_and(|raw| !raw.trim().is_empty()) {
            let now = plasm_core::temporal::temporal_reference_now()
                .map_err(|error| infrastructure_failure(error.to_string()))?;
            config.os_policy.datetime = monty_types::DateTimeSource::Fixed {
                unix_seconds: now.timestamp(),
                microsecond: now.timestamp_subsec_micros(),
            };
        }
        self.get()
            .await?
            .checkout(&config)
            .await
            .map_err(pool_failure)
    }
    #[cfg(test)]
    pub(crate) async fn compute(
        &self,
        source: String,
        rows: Records,
    ) -> Result<String, ExecutionFailure> {
        self.compute_typed(
            source,
            rows.into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|(k, v)| (k, plasm_core::Value::String(v)))
                        .collect()
                })
                .collect(),
            &BTreeMap::new(),
        )
        .await
    }
    #[cfg(test)]
    pub(crate) async fn compute_typed(
        &self,
        source: String,
        rows: TypedRecords,
        types: &BTreeMap<String, plasm_core::value_contract::ValueContract>,
    ) -> Result<String, ExecutionFailure> {
        let value = self
            .compute_value(source, rows, types, |value| {
                if value.is_string() {
                    Ok(value)
                } else {
                    Err(PythonReturnValueError::ExpectedString)
                }
            })
            .await?;
        Ok(value.as_str().expect("validated string").to_owned())
    }
    pub(crate) async fn compute_value(
        &self,
        source: String,
        rows: TypedRecords,
        types: &BTreeMap<String, plasm_core::value_contract::ValueContract>,
        validate: impl FnOnce(plasm_core::Value) -> Result<plasm_core::Value, PythonReturnValueError>,
    ) -> Result<plasm_core::Value, ExecutionFailure> {
        let mut session = self.checkout_with_suspensions(4096).await?;
        let class = MontyObject::record_type("Value", fresh_uuid());
        let input = MontyObject::list(
            rows.into_iter()
                .map(|row| {
                    let fields = row
                        .into_iter()
                        .map(|(key, value)| {
                            value_object(value, types.get(&key))
                                .map(|value| (MontyObject::string(&key), value))
                        })
                        .collect::<Result<Vec<_>, PythonValueConversionError>>()?;
                    Ok(MontyObject::class_instance(
                        class.clone(),
                        fresh_uuid(),
                        fields,
                    ))
                })
                .collect::<Result<Vec<_>, PythonValueConversionError>>()
                .map_err(|diagnostic| {
                    ExecutionFailure::new(
                        FailureCause::Program,
                        "python_input_encoding_failed",
                        diagnostic.to_string(),
                    )
                })?,
        );
        let mut inputs = vec![("__input".into(), input)];
        inputs.extend(
            crate::python_money::FUNCTIONS
                .into_iter()
                .map(|name| (name.to_owned(), MontyObject::function(name, None))),
        );
        let mut event = session
            .feed(&source, inputs, vec![], true, &mut on_print_sync(|_, _| {}))
            .await
            .map_err(program_failure)?;
        loop {
            match event {
                // Missing observed fields cannot acquire hydration authority.
                TurnEvent::NameLookup {
                    object_id: Some(_), ..
                } => {
                    event = session
                        .resume_name_lookup(None, &mut on_print_sync(|_, _| {}))
                        .await
                        .map_err(program_failure)?;
                }
                TurnEvent::FunctionCall {
                    ref function_name,
                    ref args,
                    object_id: None,
                    ..
                } if crate::python_money::FUNCTIONS.contains(&function_name.as_str()) => {
                    let result = match money_call(function_name, args) {
                        Ok(value) => monty_pool::ResumeValue::Return(
                            value_object(value, None).map_err(|diagnostic| {
                                ExecutionFailure::new(
                                    FailureCause::Runtime,
                                    "python_host_value_encoding_failed",
                                    diagnostic.to_string(),
                                )
                            })?,
                        ),
                        Err(error) => {
                            monty_pool::ResumeValue::Error(monty_types::MontyException::new(
                                monty_types::ExcType::ValueError,
                                Some(error.to_string()),
                            ))
                        }
                    };
                    event = session
                        .resume(result, &mut on_print_sync(|_, _| {}))
                        .await
                        .map_err(program_failure)?;
                }
                _ => break,
            }
        }
        let TurnEvent::Complete(output) = event else {
            return Err(ExecutionFailure::new(
                FailureCause::Program,
                "python_undeclared_interaction",
                "Pure Python compute attempted an undeclared host interaction",
            ));
        };
        let mut budget = MAX_OUTPUT;
        let result = output_value(output.as_ref(), 0, &mut budget).map_err(|diagnostic| {
            ExecutionFailure::new(
                FailureCause::Program,
                "python_output_invalid",
                diagnostic.to_string(),
            )
        })?;
        let mut remaining = MAX_OUTPUT;
        plasm_core::charge_value_budget(&result, &mut remaining).map_err(|error| {
            ExecutionFailure::new(
                FailureCause::Program,
                "python_output_budget_exceeded",
                error.to_string(),
            )
        })?;
        let result = validate(result).map_err(|detail| {
            ExecutionFailure::new(
                FailureCause::Program,
                "python_return_contract",
                detail.to_string(),
            )
        })?;
        // Only successful, validated executions return a reset worker to the pool.
        // Every error/cancellation drops the checkout and upstream kills its worker.
        session.finish().await.map_err(pool_failure)?;
        Ok(result)
    }
    pub async fn close(&self) {
        if let Some(pool) = self.pool.get() {
            pool.close().await;
        }
    }
}
/// Pool lifecycle failures belong to infrastructure, including exceptions from host-owned code.
pub(crate) fn pool_failure(error: monty_pool::PoolError) -> ExecutionFailure {
    infrastructure_failure(error.to_string())
}
fn infrastructure_failure(detail: impl Into<String>) -> ExecutionFailure {
    ExecutionFailure::new(FailureCause::Runtime, "python_pool_failure", detail)
}

/// Only the user-code execution phase grants program repair authority. Never inspect prose.
fn program_failure(error: monty_pool::PoolError) -> ExecutionFailure {
    match error {
        monty_pool::PoolError::Runtime(_) => {
            ExecutionFailure::new(FailureCause::Program, "python_exception", error.to_string())
        }
        monty_pool::PoolError::Typing(_) => ExecutionFailure::new(
            FailureCause::Program,
            "python_type_error",
            error.to_string(),
        ),
        error => pool_failure(error),
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    use plasm_runtime::RecoveryDisposition;

    #[test]
    fn python_exception_authority_comes_from_variant_and_execution_owner() {
        for kind in [
            monty_types::ExcType::ZeroDivisionError,
            monty_types::ExcType::ValueError,
            monty_types::ExcType::IndexError,
        ] {
            let error = || {
                monty_pool::PoolError::Runtime(monty_types::MontyException::new(
                    kind,
                    Some("opaque private diagnostic".into()),
                ))
            };
            let user = program_failure(error());
            assert_eq!(user.cause, FailureCause::Program);
            assert_eq!(user.recovery, RecoveryDisposition::RepairProgram);
            assert_eq!(pool_failure(error()).recovery, RecoveryDisposition::Stop);
        }
        for error in [
            monty_pool::PoolError::Exhausted,
            monty_pool::PoolError::Finished,
            monty_pool::PoolError::Protocol("ValueError: repair program".into()),
            monty_pool::PoolError::Spawn("ZeroDivisionError".into()),
        ] {
            let failure = program_failure(error);
            assert_eq!(failure.cause, FailureCause::Runtime);
            assert_eq!(failure.recovery, RecoveryDisposition::Stop);
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
        os_policy: monty_types::OsPolicy {
            random_start: monty_types::RandomStart::CallHost,
            sleep: monty_types::SleepMode::CallHost,
            ..Default::default()
        },
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
    value: plasm_core::Value,
    contract: Option<&plasm_core::value_contract::ValueContract>,
) -> Result<MontyObject, PythonValueConversionError> {
    use plasm_core::value_contract::ValueShape;
    if value.is_null() && contract.is_some_and(|t| t.nullable) {
        return Ok(MontyObject::none());
    }
    if let Some(plasm_core::value_contract::ValueContract {
        shape: ValueShape::Temporal { kind, wire },
        ..
    }) = contract
    {
        let c = plasm_core::temporal_value::components(&value, *kind, *wire)?;
        return temporal_codec::to_monty(&c, *kind);
    }
    if let Some(plasm_core::value_contract::ValueContract {
        shape: ValueShape::Union { variants },
        ..
    }) = contract
    {
        let branch = variants
            .iter()
            .find(|branch| representation_matches(branch, &value))
            .ok_or(PythonValueConversionError::UnionRepresentationMissing)?;
        return value_object(value, Some(branch));
    }
    if let (
        Some(plasm_core::value_contract::ValueContract {
            shape: ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. },
            ..
        }),
        plasm_core::Value::Object(values),
    ) = (contract, &value)
    {
        let members = values
            .iter()
            .map(|(k, v)| {
                let field = fields
                    .get(k)
                    .ok_or(PythonValueConversionError::UndeclaredRecordField)?;
                value_object(v.clone(), Some(field)).map(|v| (MontyObject::string(k), v))
            })
            .collect::<Result<Vec<_>, PythonValueConversionError>>()?;
        return Ok(MontyObject::class_instance(
            MontyObject::record_type("Record", fresh_uuid()),
            fresh_uuid(),
            members,
        ));
    }
    if let (
        Some(plasm_core::value_contract::ValueContract {
            shape:
                ValueShape::Dictionary {
                    key,
                    value: element,
                },
            ..
        }),
        plasm_core::Value::Object(values),
    ) = (contract, &value)
    {
        return Ok(MontyObject::dict(
            values
                .iter()
                .map(|(name, value)| {
                    Ok((
                        value_object(plasm_core::Value::String(name.clone()), Some(key))?,
                        value_object(value.clone(), Some(element))?,
                    ))
                })
                .collect::<Result<Vec<_>, PythonValueConversionError>>()?,
        ));
    }
    if let Some(t) = contract {
        match &t.shape {
            ValueShape::MappingRecord { record } => {
                let fields = match &record.shape {
                    ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } => {
                        fields
                    }
                    _ => return Err(PythonValueConversionError::MappingRequiresRecord),
                };
                let values = value
                    .as_object()
                    .ok_or(PythonValueConversionError::MappingRequiresObject)?;
                return Ok(MontyObject::dict(
                    values
                        .iter()
                        .map(|(k, v)| {
                            Ok((
                                MontyObject::string(k),
                                value_object(
                                    v.clone(),
                                    Some(fields.get(k).ok_or(
                                        PythonValueConversionError::UndeclaredMappingField,
                                    )?),
                                )?,
                            ))
                        })
                        .collect::<Result<Vec<_>, PythonValueConversionError>>()?,
                ));
            }
            ValueShape::Set { element } => {
                let values = value
                    .as_array()
                    .ok_or(PythonValueConversionError::SetRequiresArray)?;
                return Ok(MontyObject::set(
                    values
                        .iter()
                        .map(|v| value_object(v.clone(), Some(element)))
                        .collect::<Result<Vec<_>, PythonValueConversionError>>()?,
                ));
            }
            _ => {}
        }
    }
    Ok(match value {
        plasm_core::Value::String(s) => MontyObject::string(&s),
        plasm_core::Value::Bool(b) => MontyObject::bool(b),
        plasm_core::Value::Integer(n) => MontyObject::int(n),
        plasm_core::Value::Unsigned(n) => MontyObject::bigint(n.into()),
        plasm_core::Value::Float(n) if n.is_finite() => MontyObject::float(n),
        plasm_core::Value::Null => MontyObject::none(),
        plasm_core::Value::Array(values) => MontyObject::list(
            values
                .into_iter()
                .map(|v| {
                    value_object(
                        v,
                        contract.and_then(|t| match &t.shape {
                            ValueShape::Array { element } => Some(element.as_ref()),
                            _ => None,
                        }),
                    )
                })
                .collect::<Result<Vec<_>, PythonValueConversionError>>()?,
        ),
        plasm_core::Value::Money(m) => {
            let mut fields = vec![
                (
                    MontyObject::string("__plasm_money"),
                    MontyObject::string(m.amount().to_string()),
                ),
                (
                    MontyObject::string("currency"),
                    m.currency()
                        .map(MontyObject::string)
                        .unwrap_or_else(MontyObject::none),
                ),
            ];
            if let Some(format) = m.stored_format() {
                use plasm_core::money::MoneyWireFormat;
                let (encoding, scale) = match format {
                    MoneyWireFormat::DecimalString => ("decimal_string", None),
                    MoneyWireFormat::JsonNumber => ("json_number", None),
                    MoneyWireFormat::MinorUnits { scale } => ("minor_units", Some(scale)),
                };
                let mut format = vec![(
                    MontyObject::string("encoding"),
                    MontyObject::string(encoding),
                )];
                if let Some(scale) = scale {
                    format.push((MontyObject::string("scale"), MontyObject::int(scale.into())));
                }
                fields.push((MontyObject::string("format"), MontyObject::dict(format)));
            }
            MontyObject::dict(fields)
        }
        plasm_core::Value::Object(values) => MontyObject::dict(
            values
                .into_iter()
                .map(|(k, v)| value_object(v, None).map(|v| (MontyObject::string(&k), v)))
                .collect::<Result<Vec<_>, PythonValueConversionError>>()?,
        ),
        _ => return Err(PythonValueConversionError::UnsupportedValue),
    })
}

fn representation_matches(
    t: &plasm_core::value_contract::ValueContract,
    value: &plasm_core::Value,
) -> bool {
    use plasm_core::{value_contract::ValueShape, FieldType};
    if value.is_null() && t.nullable {
        return true;
    }
    match &t.shape {
        ValueShape::Temporal { kind, wire } => {
            plasm_core::temporal_value::components(value, *kind, *wire).is_ok()
        }
        ValueShape::Dictionary {
            key,
            value: element,
        } => value.as_object().is_some_and(|values| {
            values.iter().all(|(name, value)| {
                representation_matches(key, &plasm_core::Value::String(name.clone()))
                    && representation_matches(element, value)
            })
        }),
        ValueShape::Never => false,
        ValueShape::Null => value.is_null(),
        ValueShape::Union { variants } => variants.iter().any(|v| representation_matches(v, value)),
        ValueShape::MappingRecord { record } => representation_matches(record, value),
        ValueShape::Array { element } | ValueShape::Set { element } => value
            .as_array()
            .is_some_and(|a| a.iter().all(|v| representation_matches(element, v))),
        ValueShape::ObservedRecord {
            fields,
            optional_fields,
        } => value.as_object().is_some_and(|values| {
            values.keys().all(|name| fields.contains_key(name))
                && fields.iter().all(|(name, t)| match values.get(name) {
                    Some(value) => representation_matches(t, value),
                    None => optional_fields.contains(name),
                })
        }),
        ValueShape::Record { fields } => value.as_object().is_some_and(|values| {
            values.len() == fields.len()
                && fields.iter().all(|(name, t)| {
                    values
                        .get(name)
                        .is_some_and(|v| representation_matches(t, v))
                })
        }),
        ValueShape::Scalar { field_type } => match field_type {
            FieldType::Boolean => value.as_bool().is_some(),
            FieldType::Integer => value.as_integer().is_some() || value.as_unsigned().is_some(),
            FieldType::Number => value.is_number(),
            FieldType::Date => value.is_string() || value.as_integer().is_some(),
            FieldType::MultiSelect | FieldType::Array => value.is_array(),
            FieldType::Money => matches!(value, plasm_core::Value::Money(_)),
            FieldType::EntityRef { .. } => {
                value.as_bool().is_some()
                    || value.is_number()
                    || value.is_string()
                    || value.is_object()
            }
            FieldType::Json | FieldType::Blob => true,
            _ => value.is_string(),
        },
    }
}

/// Decode data only, with a traversal budget so cycles and shared expansion are bounded.
fn output_value(
    value: monty_types::ObjectRef<'_>,
    depth: usize,
    budget: &mut usize,
) -> Result<plasm_core::Value, PythonOutputError> {
    use plasm_core::Value as V;
    if depth >= 64 || *budget == 0 {
        return Err(PythonOutputError::BudgetExceeded);
    }
    *budget -= 1;
    if let Some(value) = temporal_codec::from_monty(value)? {
        return Ok(value);
    }
    if let Some(v) = value.as_bool() {
        return Ok(V::Bool(v));
    }
    if let Some(v) = value.as_str() {
        *budget = budget
            .checked_sub(v.len())
            .ok_or(PythonOutputError::ByteBudgetExceeded)?;
        return Ok(V::String(v.into()));
    }
    match value.type_name() {
        "NoneType" => Ok(V::Null),
        "int" => {
            if let Some(v) = value.as_int() {
                return Ok(V::Integer(v));
            }
            // The pinned boundary adapter exposes bigint limbs: bound before decimal formatting.
            let monty_types::unstable::MontyNode::BigInt(integer) =
                monty_types::unstable::node(value)
            else {
                return Err(PythonOutputError::InvalidInteger);
            };
            if integer.bits() > 64 {
                return Err(PythonOutputError::IntegerOutOfRange);
            }
            integer
                .to_string()
                .parse::<u64>()
                .map(V::Unsigned)
                .map_err(|_| PythonOutputError::IntegerOutOfRange)
        }
        "float" => {
            let n = value.as_float().ok_or(PythonOutputError::InvalidFloat)?;
            if !n.is_finite() {
                return Err(PythonOutputError::NonFiniteFloat);
            }
            Ok(V::Float(n))
        }
        "list" | "tuple" | "set" => value
            .items()
            .ok_or(PythonOutputError::InvalidCollection)?
            .into_iter()
            .map(|v| output_value(v, depth + 1, budget))
            .collect::<Result<Vec<_>, _>>()
            .map(V::Array),
        "type" | "frozenset" => Err(PythonOutputError::UnsupportedValue),
        _ => {
            if !matches!(
                monty_types::unstable::node(value),
                monty_types::unstable::MontyNode::Dict(_)
                    | monty_types::unstable::MontyNode::ClassInstance { .. }
            ) {
                return Err(PythonOutputError::UnsupportedValue);
            }
            let pairs = value.pairs().ok_or(PythonOutputError::UnsupportedValue)?;
            let mut object = indexmap::IndexMap::new();
            for (key, value) in pairs {
                let key = key.as_str().ok_or(PythonOutputError::NonStringRecordKey)?;
                *budget = budget
                    .checked_sub(key.len())
                    .ok_or(PythonOutputError::ByteBudgetExceeded)?;
                if object
                    .insert(key.into(), output_value(value, depth + 1, budget)?)
                    .is_some()
                {
                    return Err(PythonOutputError::DuplicateRecordField);
                }
            }
            Ok(V::Object(object))
        }
    }
}

/// Bind Python positional/keyword arguments once, then enter the typed kernel.
fn money_call(
    name: &str,
    args: &monty_types::CallArgs,
) -> Result<plasm_core::Value, PythonMoneyCallError> {
    let names = if matches!(name, "money_add" | "money_sub" | "money_compare") {
        ["left", "right"]
    } else {
        ["value", "factor"]
    };
    if args.args().len() > 2
        || args
            .kwargs()
            .any(|(key, _)| !names.contains(&key.as_str().unwrap_or("")))
    {
        return Err(PythonMoneyCallError::UnexpectedArguments {
            function: name.to_owned(),
        });
    }
    let mut budget = MAX_OUTPUT;
    let mut values = Vec::with_capacity(2);
    for (index, key) in names.into_iter().enumerate() {
        let positional = args.arg(index);
        let keyword = args.kwarg(key);
        if positional.is_some() && keyword.is_some() {
            return Err(PythonMoneyCallError::DuplicateArgument {
                function: name.to_owned(),
                name: key,
            });
        }
        values.push(output_value(
            positional
                .or(keyword)
                .ok_or_else(|| PythonMoneyCallError::MissingArgument {
                    function: name.to_owned(),
                    name: key,
                })?,
            0,
            &mut budget,
        )?);
    }
    let right = values.pop().expect("two bound arguments");
    Ok(crate::python_money::evaluate(
        name,
        values.pop().expect("two bound arguments"),
        right,
    )?)
}
