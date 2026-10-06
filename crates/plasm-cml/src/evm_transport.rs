//! EVM transport types and compilation logic.
//!
//! This module is compiled only when the `evm` feature is enabled.
//! It owns all Alloy-backed types (`CompiledEvmCall`, `CompiledEvmLogs`, etc.)
//! and the `compile_evm_call` / `compile_evm_logs` routines that transform
//! YAML-sourced templates into concrete on-chain requests.

use crate::cml::{eval_cml, CmlEnv, CmlExpr, PaginationConfig};
use crate::error::CmlError;
use alloy_chains::Chain;
use alloy_dyn_abi::{DynSolType, DynSolValue};
use alloy_json_abi::{Event, Function};
use alloy_primitives::{Address, B256};
use alloy_rpc_types::BlockNumberOrTag;
use indexmap::IndexMap;
use plasm_core::Value;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;

#[derive(Debug, Clone, thiserror::Error)]
pub enum EvmCompileError {
    #[error("invalid function signature '{signature}': {source}")]
    FunctionSignature {
        signature: String,
        #[source]
        source: <Function as FromStr>::Err,
    },
    #[error("function '{signature}' expects {expected} args but template provides {actual}")]
    ArgumentCount {
        signature: String,
        expected: usize,
        actual: usize,
    },
    #[error("invalid event signature '{signature}': {source}")]
    EventSignature {
        signature: String,
        #[source]
        source: <Event as FromStr>::Err,
    },
    #[error("event '{event}' has {expected} indexed inputs but template provides {actual} topic filters")]
    IndexedTopicCount {
        event: String,
        expected: usize,
        actual: usize,
    },
    #[error("EVM log filters support at most {maximum} topic filters (anonymous={anonymous}), but template provides {actual}")]
    TopicLimit {
        maximum: usize,
        anonymous: bool,
        actual: usize,
    },
    #[error("invalid indexed event type '{solidity_type}': {source}")]
    IndexedType {
        solidity_type: String,
        #[source]
        source: <DynSolType as FromStr>::Err,
    },
    #[error("indexed event filter '{parameter}' is not word-encodable")]
    IndexedFilterNotWord { parameter: String },
    #[error(
        "Plasm compile-time input references cannot be coerced to solidity type '{solidity_type}'"
    )]
    UnboundOperand { solidity_type: DynSolType },
    #[error("failed to coerce {value:?} to solidity type '{solidity_type}': {source}")]
    SolidityCoercion {
        solidity_type: DynSolType,
        value: Value,
        #[source]
        source: alloy_dyn_abi::Error,
    },
    #[error("cannot coerce null to solidity type '{solidity_type}'")]
    NullSolidityValue { solidity_type: DynSolType },
    #[error("complex CML values ({actual}) are not yet supported for solidity type coercion ('{solidity_type}')")]
    ComplexSolidityValue {
        solidity_type: DynSolType,
        actual: &'static str,
    },
    #[error("invalid chain '{chain}': {source}")]
    Chain {
        chain: String,
        #[source]
        source: <Chain as FromStr>::Err,
    },
    #[error("invalid address '{address}': {source}")]
    Address {
        address: String,
        #[source]
        source: <Address as FromStr>::Err,
    },
    #[error("expected address string, found {actual}")]
    AddressType { actual: &'static str },
    #[error("block number must be non-negative, got {number}")]
    NegativeBlockNumber { number: i64 },
    #[error("invalid block tag '{tag}': {source}")]
    BlockTag {
        tag: String,
        #[source]
        source: Arc<<BlockNumberOrTag as FromStr>::Err>,
    },
    #[error("expected block tag or number, found {actual}")]
    BlockType { actual: &'static str },
}

// ---------------------------------------------------------------------------
// Template types (schema-sourced, pre-compilation)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvmCallTemplate {
    pub chain: ChainLiteral,
    pub contract: CmlExpr,
    pub function: String,
    #[serde(default)]
    pub args: Vec<CmlExpr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<CmlExpr>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub decode: IndexMap<String, EvmFieldSource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvmLogsTemplate {
    pub chain: ChainLiteral,
    pub contract: CmlExpr,
    pub event: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<CmlExpr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagination: Option<PaginationConfig>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub decode: IndexMap<String, EvmFieldSource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChainLiteral {
    Named(String),
    Id(u64),
}

// ---------------------------------------------------------------------------
// Compiled types (runtime-ready)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledEvmCall {
    pub chain: Chain,
    pub contract: Address,
    pub function: Function,
    pub args: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockNumberOrTag>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub decode: IndexMap<String, EvmFieldSource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledEvmLogs {
    pub chain: Chain,
    pub contract: Address,
    pub event: Event,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexed_filters: Vec<Option<B256>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_block: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_block: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagination: Option<PaginationConfig>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub decode: IndexMap<String, EvmFieldSource>,
}

// ---------------------------------------------------------------------------
// Decode descriptors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvmFieldSource {
    Input { index: usize },
    Output { index: usize },
    Topic { index: usize },
    Data { index: usize },
    LogMeta { key: EvmLogMetaKey },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvmLogMetaKey {
    Address,
    BlockNumber,
    EventId,
    TransactionHash,
    LogIndex,
    Removed,
}

// ---------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------

pub fn compile_evm_call(
    template: &EvmCallTemplate,
    env: &CmlEnv,
) -> Result<CompiledEvmCall, CmlError> {
    let chain = parse_chain(&template.chain)?;
    let contract = eval_address(&template.contract, env)?;
    let function = template.function.parse::<Function>().map_err(|source| {
        EvmCompileError::FunctionSignature {
            signature: template.function.clone(),
            source,
        }
    })?;

    if function.inputs.len() != template.args.len() {
        return Err(EvmCompileError::ArgumentCount {
            signature: template.function.clone(),
            expected: function.inputs.len(),
            actual: template.args.len(),
        }
        .into());
    }

    let mut args = Vec::with_capacity(template.args.len());
    for arg in &template.args {
        args.push(eval_cml(arg, env)?);
    }

    let block = template
        .block
        .as_ref()
        .map(|expr| eval_block(expr, env))
        .transpose()?;

    Ok(CompiledEvmCall {
        chain,
        contract,
        function,
        args,
        block,
        decode: template.decode.clone(),
    })
}

pub fn compile_evm_logs(
    template: &EvmLogsTemplate,
    env: &CmlEnv,
) -> Result<CompiledEvmLogs, CmlError> {
    let chain = parse_chain(&template.chain)?;
    let contract = eval_address(&template.contract, env)?;
    let event =
        template
            .event
            .parse::<Event>()
            .map_err(|source| EvmCompileError::EventSignature {
                signature: template.event.clone(),
                source,
            })?;

    let indexed_inputs: Vec<_> = event.inputs.iter().filter(|param| param.indexed).collect();
    let max_user_topics = if event.anonymous { 4 } else { 3 };
    if template.topics.len() > indexed_inputs.len() {
        return Err(EvmCompileError::IndexedTopicCount {
            event: template.event.clone(),
            expected: indexed_inputs.len(),
            actual: template.topics.len(),
        }
        .into());
    }
    if template.topics.len() > max_user_topics {
        return Err(EvmCompileError::TopicLimit {
            maximum: max_user_topics,
            anonymous: event.anonymous,
            actual: template.topics.len(),
        }
        .into());
    }

    let mut indexed_filters = Vec::with_capacity(indexed_inputs.len());
    for (idx, input) in indexed_inputs.iter().enumerate() {
        let maybe_expr = template.topics.get(idx);
        let maybe_value = maybe_expr.map(|expr| eval_cml(expr, env)).transpose()?;
        let filter =
            match maybe_value {
                None | Some(Value::Null) => None,
                Some(value) => {
                    let ty = input.ty.parse::<DynSolType>().map_err(|source| {
                        EvmCompileError::IndexedType {
                            solidity_type: input.ty.clone(),
                            source,
                        }
                    })?;
                    let dyn_value = coerce_dyn_value(&value, &ty)?;
                    Some(dyn_value.as_word().ok_or_else(|| {
                        EvmCompileError::IndexedFilterNotWord {
                            parameter: input.name.clone(),
                        }
                    })?)
                }
            };
        indexed_filters.push(filter);
    }

    Ok(CompiledEvmLogs {
        chain,
        contract,
        event,
        indexed_filters,
        from_block: None,
        to_block: None,
        pagination: template.pagination.clone(),
        decode: template.decode.clone(),
    })
}

// ---------------------------------------------------------------------------
// Solidity type coercion (also used by plasm-runtime/src/evm.rs)
// ---------------------------------------------------------------------------

pub fn coerce_dyn_value(value: &Value, ty: &DynSolType) -> Result<DynSolValue, CmlError> {
    let coerce = |text: &str| {
        ty.coerce_str(text).map_err(|source| {
            CmlError::from(EvmCompileError::SolidityCoercion {
                solidity_type: ty.clone(),
                value: value.clone(),
                source,
            })
        })
    };
    match value {
        Value::PlasmInputRef(_) | Value::GetScalarExtract(_) | Value::StringTemplate(_) => {
            Err(EvmCompileError::UnboundOperand {
                solidity_type: ty.clone(),
            }
            .into())
        }
        Value::String(s) | Value::PhraseIdent(s) => coerce(s),
        Value::Integer(i) => coerce(&i.to_string()),
        Value::Unsigned(i) => coerce(&i.to_string()),
        Value::Float(f) => coerce(&f.to_string()),
        Value::Bool(b) => coerce(&b.to_string()),
        Value::Null => Err(EvmCompileError::NullSolidityValue {
            solidity_type: ty.clone(),
        }
        .into()),
        Value::Array(_) | Value::Object(_) | Value::UnionCtor { .. } | Value::Money(_) => {
            Err(EvmCompileError::ComplexSolidityValue {
                solidity_type: ty.clone(),
                actual: value.type_name(),
            }
            .into())
        }
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn parse_chain(chain: &ChainLiteral) -> Result<Chain, CmlError> {
    match chain {
        ChainLiteral::Named(name) => name.parse::<Chain>().map_err(|source| {
            EvmCompileError::Chain {
                chain: name.clone(),
                source,
            }
            .into()
        }),
        ChainLiteral::Id(id) => Ok((*id).into()),
    }
}

fn eval_address(expr: &CmlExpr, env: &CmlEnv) -> Result<Address, CmlError> {
    let value = eval_cml(expr, env)?;
    match value {
        Value::String(s) => s
            .parse::<Address>()
            .map_err(|source| EvmCompileError::Address { address: s, source }.into()),
        other => Err(EvmCompileError::AddressType {
            actual: other.type_name(),
        }
        .into()),
    }
}

fn eval_block(expr: &CmlExpr, env: &CmlEnv) -> Result<BlockNumberOrTag, CmlError> {
    let value = eval_cml(expr, env)?;
    match value {
        Value::Integer(i) if i >= 0 => Ok((i as u64).into()),
        Value::Integer(i) => Err(EvmCompileError::NegativeBlockNumber { number: i }.into()),
        Value::String(s) => {
            if let Ok(n) = s.parse::<u64>() {
                Ok(n.into())
            } else {
                s.parse::<BlockNumberOrTag>().map_err(|source| {
                    EvmCompileError::BlockTag {
                        tag: s,
                        source: Arc::new(source),
                    }
                    .into()
                })
            }
        }
        other => Err(EvmCompileError::BlockType {
            actual: other.type_name(),
        }
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn solidity_failure_retains_type_value_and_alloy_source() {
        let ty = DynSolType::Uint(256);
        let error = coerce_dyn_value(&Value::String("not-a-number".into()), &ty).unwrap_err();
        let CmlError::Evm(source) = &error else {
            panic!("expected EVM coercion failure: {error}");
        };
        assert!(
            matches!(source.as_ref(), EvmCompileError::SolidityCoercion {
                solidity_type: DynSolType::Uint(256), value: Value::String(value), ..
            } if value == "not-a-number")
        );
        assert!(error
            .source()
            .unwrap()
            .downcast_ref::<alloy_dyn_abi::Error>()
            .is_some());
        let cloned = error.clone();
        assert_eq!(cloned.to_string(), error.to_string());
        assert!(cloned
            .source()
            .unwrap()
            .downcast_ref::<alloy_dyn_abi::Error>()
            .is_some());
    }

    #[test]
    fn evm_address_and_block_rejections_are_semantic() {
        let env = CmlEnv::new();
        let error = eval_address(&CmlExpr::const_("not-an-address"), &env).unwrap_err();
        let CmlError::Evm(source) = &error else {
            panic!("expected EVM address failure: {error}");
        };
        assert!(
            matches!(source.as_ref(), EvmCompileError::Address { address, .. } if address == "not-an-address")
        );
        assert!(error
            .source()
            .unwrap()
            .downcast_ref::<<Address as FromStr>::Err>()
            .is_some());
        let error = eval_block(&CmlExpr::const_(-1_i64), &env).unwrap_err();
        let CmlError::Evm(source) = &error else {
            panic!("expected EVM block failure: {error}");
        };
        assert!(matches!(
            source.as_ref(),
            EvmCompileError::NegativeBlockNumber { number: -1 }
        ));
    }
}
