use crate::{ResolvedAuth, RuntimeError};
use alloy_dyn_abi::{DynSolValue, EventExt, FunctionExt, JsonAbiExt};
use alloy_primitives::{hex, I256, U256};
use alloy_provider::{DynProvider, Provider, ProviderBuilder};
use alloy_rpc_types::{BlockId, Filter, Log, TransactionRequest};
use alloy_transport_http::reqwest as alloy_reqwest;
use plasm_compile::{
    coerce_dyn_value, CompiledEvmCall, CompiledEvmLogs, EvmFieldSource, EvmLogMetaKey,
};
use serde_json::{Map, Value as JsonValue};

pub async fn execute_evm_call(
    rpc_url: &str,
    auth: Option<&ResolvedAuth>,
    op: &CompiledEvmCall,
) -> Result<serde_json::Value, RuntimeError> {
    let provider = connect_provider(rpc_url, auth)?;
    let args = compile_call_args(op)?;
    let calldata = op.function.abi_encode_input(&args).map_err(|e| {
        request_error(format!(
            "failed to ABI-encode EVM call '{}': {e}",
            op.function.signature()
        ))
    })?;

    let tx = TransactionRequest {
        to: Some(op.contract.into()),
        input: calldata.into(),
        chain_id: Some(op.chain.id()),
        ..Default::default()
    };

    let raw = match op.block {
        Some(block) => provider.call(tx).block(BlockId::Number(block)).await,
        None => provider.call(tx).await,
    }
    .map_err(|e| {
        request_error(format!(
            "eth_call failed for '{}': {e}",
            op.function.signature()
        ))
    })?;

    let outputs = op.function.abi_decode_output(raw.as_ref()).map_err(|e| {
        request_error(format!(
            "failed to decode EVM call output '{}': {e}",
            op.function.signature()
        ))
    })?;

    normalize_call_response(op, &outputs)
}

pub async fn execute_evm_logs(
    rpc_url: &str,
    auth: Option<&ResolvedAuth>,
    op: &CompiledEvmLogs,
) -> Result<serde_json::Value, RuntimeError> {
    let provider = connect_provider(rpc_url, auth)?;
    let filter = build_log_filter(op);
    let logs = provider.get_logs(&filter).await.map_err(|e| {
        request_error(format!(
            "eth_getLogs failed for '{}': {e}",
            op.event.signature()
        ))
    })?;

    let mut rows = Vec::with_capacity(logs.len());
    for log in &logs {
        rows.push(normalize_log_response(op, log)?);
    }
    Ok(JsonValue::Array(rows))
}

fn connect_provider(
    rpc_url: &str,
    auth: Option<&ResolvedAuth>,
) -> Result<DynProvider, RuntimeError> {
    let mut url =
        alloy_reqwest::Url::parse(rpc_url).map_err(|e| RuntimeError::ConfigurationError {
            message: format!("invalid EVM RPC URL '{rpc_url}': {e}"),
        })?;
    if let Some(auth) = auth {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in &auth.query_params {
            pairs.append_pair(key, value);
        }
    }
    let client = build_http_client(auth)?;
    Ok(ProviderBuilder::new().connect_reqwest(client, url).erased())
}

fn build_http_client(auth: Option<&ResolvedAuth>) -> Result<alloy_reqwest::Client, RuntimeError> {
    let mut builder = alloy_reqwest::Client::builder();
    if let Some(auth) = auth {
        let headers = build_default_headers(&auth.headers)?;
        if !headers.is_empty() {
            builder = builder.default_headers(headers);
        }
    }
    builder
        .build()
        .map_err(|e| RuntimeError::ConfigurationError {
            message: format!("failed to build EVM RPC client: {e}"),
        })
}

fn build_default_headers(
    entries: &[(String, String)],
) -> Result<alloy_reqwest::header::HeaderMap, RuntimeError> {
    let mut headers = alloy_reqwest::header::HeaderMap::new();
    for (key, value) in entries {
        let name = alloy_reqwest::header::HeaderName::from_bytes(key.as_bytes()).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: format!("invalid EVM RPC auth header '{key}': {e}"),
            }
        })?;
        let value = alloy_reqwest::header::HeaderValue::from_str(value).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: format!("invalid value for EVM RPC auth header '{key}': {e}"),
            }
        })?;
        headers.append(name, value);
    }
    Ok(headers)
}

fn compile_call_args(op: &CompiledEvmCall) -> Result<Vec<DynSolValue>, RuntimeError> {
    let mut args = Vec::with_capacity(op.args.len());
    for (value, param) in op.args.iter().zip(&op.function.inputs) {
        let ty = param
            .ty
            .parse()
            .map_err(|e| RuntimeError::ConfigurationError {
                message: format!(
                    "invalid ABI input type '{}' for '{}': {e}",
                    param.ty,
                    op.function.signature()
                ),
            })?;
        args.push(coerce_dyn_value(value, &ty)?);
    }
    Ok(args)
}

fn build_log_filter(op: &CompiledEvmLogs) -> Filter {
    let mut filter = Filter::new().address(op.contract);
    if let Some(from_block) = op.from_block {
        filter = filter.from_block(from_block);
    }
    if let Some(to_block) = op.to_block {
        filter = filter.to_block(to_block);
    }
    if !op.event.anonymous {
        filter = filter.event_signature(op.event.selector());
    }

    let topic_offset = usize::from(!op.event.anonymous);
    for (idx, topic) in op.indexed_filters.iter().enumerate() {
        if let Some(topic) = topic {
            let slot = idx + topic_offset;
            if slot < filter.topics.len() {
                filter.topics[slot] = (*topic).into();
            }
        }
    }

    filter
}

fn normalize_call_response(
    op: &CompiledEvmCall,
    outputs: &[DynSolValue],
) -> Result<JsonValue, RuntimeError> {
    if op.decode.is_empty() {
        return Ok(match outputs {
            [single] => dyn_value_to_json(single),
            many => JsonValue::Array(many.iter().map(dyn_value_to_json).collect()),
        });
    }

    let mut obj = Map::new();
    for (field, source) in &op.decode {
        obj.insert(field.clone(), extract_call_field(source, op, outputs)?);
    }
    Ok(JsonValue::Object(obj))
}

fn normalize_log_response(op: &CompiledEvmLogs, log: &Log) -> Result<JsonValue, RuntimeError> {
    let decoded = op.event.decode_log(log.data()).map_err(|e| {
        request_error(format!(
            "failed to decode log for event '{}': {e}",
            op.event.signature()
        ))
    })?;

    if op.decode.is_empty() {
        let mut obj = Map::new();
        obj.insert(
            "address".to_string(),
            JsonValue::String(log.address().to_string()),
        );
        obj.insert(
            "indexed".to_string(),
            JsonValue::Array(decoded.indexed.iter().map(dyn_value_to_json).collect()),
        );
        obj.insert(
            "data".to_string(),
            JsonValue::Array(decoded.body.iter().map(dyn_value_to_json).collect()),
        );
        if let Some(block_number) = log.block_number {
            obj.insert("block_number".to_string(), u64_to_json(block_number));
        }
        if let Some(tx_hash) = log.transaction_hash {
            obj.insert(
                "transaction_hash".to_string(),
                JsonValue::String(tx_hash.to_string()),
            );
        }
        if let Some(log_index) = log.log_index {
            obj.insert("log_index".to_string(), u64_to_json(log_index));
        }
        return Ok(JsonValue::Object(obj));
    }

    let mut obj = Map::new();
    for (field, source) in &op.decode {
        obj.insert(
            field.clone(),
            extract_log_field(source, log, &decoded.indexed, &decoded.body)?,
        );
    }
    Ok(JsonValue::Object(obj))
}

fn extract_call_field(
    source: &EvmFieldSource,
    op: &CompiledEvmCall,
    outputs: &[DynSolValue],
) -> Result<JsonValue, RuntimeError> {
    match source {
        EvmFieldSource::Input { index } => op
            .args
            .get(*index)
            .map(plasm_value_to_json)
            .transpose()?
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!(
                    "decode.input index {} out of bounds for '{}'",
                    index,
                    op.function.signature()
                ),
            }),
        EvmFieldSource::Output { index } => {
            outputs.get(*index).map(dyn_value_to_json).ok_or_else(|| {
                RuntimeError::ConfigurationError {
                    message: format!(
                        "decode.output index {} out of bounds for '{}'",
                        index,
                        op.function.signature()
                    ),
                }
            })
        }
        EvmFieldSource::Topic { .. }
        | EvmFieldSource::Data { .. }
        | EvmFieldSource::LogMeta { .. } => Err(RuntimeError::ConfigurationError {
            message: format!(
                "EVM call decode for '{}' only supports input/output sources",
                op.function.signature()
            ),
        }),
    }
}

fn extract_log_field(
    source: &EvmFieldSource,
    log: &Log,
    indexed: &[DynSolValue],
    body: &[DynSolValue],
) -> Result<JsonValue, RuntimeError> {
    match source {
        EvmFieldSource::Topic { index } => {
            indexed.get(*index).map(dyn_value_to_json).ok_or_else(|| {
                RuntimeError::ConfigurationError {
                    message: format!("decode.topic index {index} out of bounds for EVM log"),
                }
            })
        }
        EvmFieldSource::Data { index } => {
            body.get(*index).map(dyn_value_to_json).ok_or_else(|| {
                RuntimeError::ConfigurationError {
                    message: format!("decode.data index {index} out of bounds for EVM log"),
                }
            })
        }
        EvmFieldSource::LogMeta { key } => extract_log_meta(*key, log),
        EvmFieldSource::Input { .. } | EvmFieldSource::Output { .. } => {
            Err(RuntimeError::ConfigurationError {
                message: "EVM log decode only supports topic/data/log_meta sources".to_string(),
            })
        }
    }
}

fn extract_log_meta(key: EvmLogMetaKey, log: &Log) -> Result<JsonValue, RuntimeError> {
    let value = match key {
        EvmLogMetaKey::Address => JsonValue::String(log.address().to_string()),
        EvmLogMetaKey::BlockNumber => log.block_number.map(u64_to_json).unwrap_or(JsonValue::Null),
        EvmLogMetaKey::EventId => {
            let tx_hash = log
                .transaction_hash
                .ok_or_else(|| RuntimeError::RequestError {
                    message: "EVM log is missing transaction_hash required for event_id"
                        .to_string(),
                    attempts: 1,
                    status: None,
                    body: None,
                })?;
            let log_index = log.log_index.ok_or_else(|| RuntimeError::RequestError {
                message: "EVM log is missing log_index required for event_id".to_string(),
                attempts: 1,
                status: None,
                body: None,
            })?;
            JsonValue::String(format!("{tx_hash}:{log_index}"))
        }
        EvmLogMetaKey::TransactionHash => log
            .transaction_hash
            .map(|hash| JsonValue::String(hash.to_string()))
            .unwrap_or(JsonValue::Null),
        EvmLogMetaKey::LogIndex => log.log_index.map(u64_to_json).unwrap_or(JsonValue::Null),
        EvmLogMetaKey::Removed => JsonValue::Bool(log.removed),
    };
    Ok(value)
}

fn dyn_value_to_json(value: &DynSolValue) -> JsonValue {
    match value {
        DynSolValue::Bool(b) => JsonValue::Bool(*b),
        DynSolValue::Int(i, _) => JsonValue::String(i256_to_string(*i)),
        DynSolValue::Uint(u, _) => JsonValue::String(u.to_string()),
        DynSolValue::FixedBytes(bytes, size) => {
            JsonValue::String(format!("0x{}", hex::encode(&bytes[..*size])))
        }
        DynSolValue::Address(address) => JsonValue::String(address.to_string()),
        DynSolValue::Function(function) => {
            JsonValue::String(format!("0x{}", hex::encode(function.as_slice())))
        }
        DynSolValue::Bytes(bytes) => JsonValue::String(format!("0x{}", hex::encode(bytes))),
        DynSolValue::String(s) => JsonValue::String(s.clone()),
        DynSolValue::Array(values)
        | DynSolValue::FixedArray(values)
        | DynSolValue::Tuple(values) => {
            JsonValue::Array(values.iter().map(dyn_value_to_json).collect())
        }
    }
}

fn i256_to_string(value: I256) -> String {
    if value.is_negative() {
        let magnitude: U256 = value.wrapping_abs().into_raw();
        format!("-{magnitude}")
    } else {
        value.into_raw().to_string()
    }
}

fn u64_to_json(value: u64) -> JsonValue {
    match i64::try_from(value) {
        Ok(value) => JsonValue::Number(value.into()),
        Err(_) => JsonValue::String(value.to_string()),
    }
}

fn plasm_value_to_json(value: &plasm_core::Value) -> Result<JsonValue, RuntimeError> {
    serde_json::to_value(value).map_err(|e| RuntimeError::SerializationError {
        message: e.to_string(),
    })
}

fn request_error(message: String) -> RuntimeError {
    RuntimeError::RequestError {
        message,
        attempts: 1,
        status: None,
        body: None,
    }
}
