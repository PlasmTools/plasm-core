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
    let function =
        template
            .function
            .parse::<Function>()
            .map_err(|e| CmlError::InvalidTemplate {
                message: format!("invalid function signature '{}': {e}", template.function),
            })?;

    if function.inputs.len() != template.args.len() {
        return Err(CmlError::InvalidTemplate {
            message: format!(
                "function '{}' expects {} args but template provides {}",
                template.function,
                function.inputs.len(),
                template.args.len()
            ),
        });
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
    let event = template
        .event
        .parse::<Event>()
        .map_err(|e| CmlError::InvalidTemplate {
            message: format!(
                "invalid event signature '{event}': {e}",
                event = template.event
            ),
        })?;

    let indexed_inputs: Vec<_> = event.inputs.iter().filter(|param| param.indexed).collect();
    let max_user_topics = if event.anonymous { 4 } else { 3 };
    if template.topics.len() > indexed_inputs.len() {
        return Err(CmlError::InvalidTemplate {
            message: format!(
                "event '{}' has {} indexed inputs but template provides {} topic filters",
                template.event,
                indexed_inputs.len(),
                template.topics.len()
            ),
        });
    }
    if template.topics.len() > max_user_topics {
        return Err(CmlError::InvalidTemplate {
            message: format!(
                "EVM log filters support at most {max_user_topics} topic filters \
                 ({} event), but template provides {}",
                if event.anonymous {
                    "anonymous"
                } else {
                    "non-anonymous"
                },
                template.topics.len()
            ),
        });
    }

    let mut indexed_filters = Vec::with_capacity(indexed_inputs.len());
    for (idx, input) in indexed_inputs.iter().enumerate() {
        let maybe_expr = template.topics.get(idx);
        let maybe_value = maybe_expr.map(|expr| eval_cml(expr, env)).transpose()?;
        let filter = match maybe_value {
            None | Some(Value::Null) => None,
            Some(value) => {
                let ty = input
                    .ty
                    .parse::<DynSolType>()
                    .map_err(|e| CmlError::InvalidTemplate {
                        message: format!("invalid indexed event type '{}': {e}", input.ty),
                    })?;
                let dyn_value = coerce_dyn_value(&value, &ty)?;
                Some(
                    dyn_value
                        .as_word()
                        .ok_or_else(|| CmlError::InvalidTemplate {
                            message: format!(
                                "indexed event filter '{}' is not word-encodable",
                                input.name
                            ),
                        })?,
                )
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
    match value {
        Value::PlasmInputRef(_) => Err(CmlError::TypeError {
            message: format!(
                "Plasm compile-time input references cannot be coerced to solidity type '{ty}'"
            ),
        }),
        Value::String(s) | Value::PhraseIdent(s) => ty.coerce_str(s).map_err(|e| CmlError::EvaluationError {
            message: format!("failed to coerce '{s}' to solidity type '{ty}': {e}"),
        }),
        Value::Integer(i) => ty
            .coerce_str(&i.to_string())
            .map_err(|e| CmlError::EvaluationError {
                message: format!("failed to coerce integer '{i}' to solidity type '{ty}': {e}"),
            }),
        Value::Float(f) => ty
            .coerce_str(&f.to_string())
            .map_err(|e| CmlError::EvaluationError {
                message: format!("failed to coerce float '{f}' to solidity type '{ty}': {e}"),
            }),
        Value::Bool(b) => ty
            .coerce_str(&b.to_string())
            .map_err(|e| CmlError::EvaluationError {
                message: format!("failed to coerce bool '{b}' to solidity type '{ty}': {e}"),
            }),
        Value::Null => Err(CmlError::EvaluationError {
            message: format!("cannot coerce null to solidity type '{ty}'"),
        }),
        Value::Array(_) | Value::Object(_) | Value::UnionCtor { .. } => Err(CmlError::TypeError {
            message: format!(
                "complex CML values are not yet supported for solidity type coercion ('{ty}')"
            ),
        }),
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn parse_chain(chain: &ChainLiteral) -> Result<Chain, CmlError> {
    match chain {
        ChainLiteral::Named(name) => name
            .parse::<Chain>()
            .map_err(|e| CmlError::InvalidTemplate {
                message: format!("invalid chain '{name}': {e}"),
            }),
        ChainLiteral::Id(id) => Ok((*id).into()),
    }
}

fn eval_address(expr: &CmlExpr, env: &CmlEnv) -> Result<Address, CmlError> {
    let value = eval_cml(expr, env)?;
    match value {
        Value::String(s) => s.parse::<Address>().map_err(|e| CmlError::EvaluationError {
            message: format!("invalid address '{s}': {e}"),
        }),
        other => Err(CmlError::TypeError {
            message: format!("expected address string, found {}", other.type_name()),
        }),
    }
}

fn eval_block(expr: &CmlExpr, env: &CmlEnv) -> Result<BlockNumberOrTag, CmlError> {
    let value = eval_cml(expr, env)?;
    match value {
        Value::Integer(i) if i >= 0 => Ok((i as u64).into()),
        Value::Integer(i) => Err(CmlError::EvaluationError {
            message: format!("block number must be non-negative, got {i}"),
        }),
        Value::String(s) => {
            if let Ok(n) = s.parse::<u64>() {
                Ok(n.into())
            } else {
                s.parse::<BlockNumberOrTag>()
                    .map_err(|e| CmlError::EvaluationError {
                        message: format!("invalid block tag '{s}': {e}"),
                    })
            }
        }
        other => Err(CmlError::TypeError {
            message: format!("expected block tag or number, found {}", other.type_name()),
        }),
    }
}
