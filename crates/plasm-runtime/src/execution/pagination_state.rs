//! Pagination loop state machine and CML request param injection.

use super::{json_to_plasm_value, QueryPaginationState, RuntimeError, StreamConsumeOpts};
use indexmap::IndexMap;
use plasm_compile::{CompiledOperation, CompiledRequest, PaginationConfig};
use plasm_core::{QueryPagination, Value};

fn pagination_default_limit(pconf: &PaginationConfig) -> u32 {
    let size_names = [
        "size",
        "limit",
        "per_page",
        "page_size",
        "maxResults",
        "max_results",
        "first",
        "$top",
        "top",
    ];
    for (name, param) in &pconf.params {
        let is_size_like = size_names.contains(&name.as_str())
            || name.ends_with("_size")
            || name.ends_with("_limit");
        if is_size_like {
            if let Some(v) = param.fixed_as_u32() {
                return v.max(1);
            }
        }
    }
    // BlockRange: look for a fixed range_size param
    if pconf.location == plasm_compile::PaginationLocation::BlockRange {
        for param in pconf.params.values() {
            if let Some(v) = param.fixed_as_u32() {
                return v.max(1);
            }
        }
        return 1000; // default block range span
    }
    20
}

// Legacy stub kept for compatibility with the compile-layer import.
#[allow(dead_code)]
fn pagination_default_limit_stub() -> u32 {
    // Placeholder to satisfy any residual references.
    20
}

fn _pagination_items_key_unused() {
    // Removed: items key now comes from cml_request.response.items (HttpResponseDecode)
    // not from PaginationConfig.
}

fn compiled_query_insert_http(compiled: &mut CompiledRequest, key: &str, val: Value) {
    use indexmap::IndexMap;
    if compiled.query.is_none() {
        compiled.query = Some(Value::Object(IndexMap::new()));
    }
    if let Some(Value::Object(m)) = compiled.query.as_mut() {
        m.insert(key.to_string(), val);
    } else {
        let mut m = IndexMap::new();
        m.insert(key.to_string(), val);
        compiled.query = Some(Value::Object(m));
    }
}

fn compiled_query_insert(
    compiled: &mut CompiledOperation,
    key: &str,
    val: Value,
) -> Result<(), RuntimeError> {
    match compiled {
        CompiledOperation::Http(request) | CompiledOperation::GraphQl(request) => {
            compiled_query_insert_http(request, key, val);
            Ok(())
        }
        CompiledOperation::EvmCall(_) => Err(RuntimeError::ConfigurationError {
            message: format!("pagination key '{key}' is not valid for evm_call transport"),
        }),
        CompiledOperation::EvmLogs(_) => Err(RuntimeError::ConfigurationError {
            message: format!(
                "query parameter pagination key '{key}' is not valid for evm_logs transport"
            ),
        }),
        CompiledOperation::View(_) => Err(RuntimeError::ConfigurationError {
            message: format!("pagination key '{key}' is not valid for composed view transport"),
        }),
    }
}

fn compiled_block_range_set(
    compiled: &mut CompiledOperation,
    from_block: u64,
    to_block: u64,
) -> Result<(), RuntimeError> {
    match compiled {
        CompiledOperation::EvmLogs(request) => {
            request.from_block = Some(from_block);
            request.to_block = Some(to_block);
            Ok(())
        }
        CompiledOperation::Http(request) | CompiledOperation::GraphQl(request) => {
            compiled_query_insert_http(
                request,
                "from_block",
                Value::String(from_block.to_string()),
            );
            compiled_query_insert_http(request, "to_block", Value::String(to_block.to_string()));
            Ok(())
        }
        CompiledOperation::EvmCall(_) => Err(RuntimeError::ConfigurationError {
            message: "block-range pagination is not valid for evm_call transport".to_string(),
        }),
        CompiledOperation::View(_) => Err(RuntimeError::ConfigurationError {
            message: "block-range pagination is not valid for composed view transport".to_string(),
        }),
    }
}

/// Merge one pagination key into the compiled JSON body: either at the root object or under
/// [`PaginationConfig::body_merge_path`] (GraphQL `variables.…` nesting).
pub(crate) fn merge_pagination_into_body(
    body: &mut Value,
    merge_path: Option<&[String]>,
    key: &str,
    value: Value,
) -> Result<(), RuntimeError> {
    let target_map: &mut IndexMap<String, Value> =
        if let Some(path) = merge_path.filter(|p| !p.is_empty()) {
            let Value::Object(root) = body else {
                return Err(RuntimeError::ConfigurationError {
                    message: "pagination with body_merge_path requires a JSON object request body"
                        .into(),
                });
            };
            let mut cur = root;
            for segment in path {
                let entry = cur
                    .entry(segment.clone())
                    .or_insert_with(|| Value::Object(IndexMap::new()));
                match entry {
                    Value::Object(next) => cur = next,
                    _ => {
                        return Err(RuntimeError::ConfigurationError {
                            message: format!(
                                "pagination body_merge_path: expected object at segment '{segment}'"
                            ),
                        });
                    }
                }
            }
            cur
        } else {
            let Value::Object(m) = body else {
                return Err(RuntimeError::ConfigurationError {
                    message: "pagination body injection requires a JSON object request body".into(),
                });
            };
            m
        };
    target_map.insert(key.to_string(), value);
    Ok(())
}

fn response_map(
    v: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, RuntimeError> {
    match v {
        serde_json::Value::Object(m) => Ok(m),
        _ => Err(RuntimeError::ConfigurationError {
            message: "expected JSON object in paginated API response".into(),
        }),
    }
}

/// Object map used for `stop_when` and `FromResponse` pagination keys.
/// When `prefix` is `None` or empty, uses the root JSON object.
pub(crate) fn pagination_context_map<'a>(
    response: &'a serde_json::Value,
    prefix: Option<&[String]>,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, RuntimeError> {
    let mut cur = response;
    if let Some(segs) = prefix.filter(|p| !p.is_empty()) {
        for seg in segs {
            cur = if let Ok(index) = seg.parse::<usize>() {
                cur.get(index)
            } else {
                cur.get(seg)
            }
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!("pagination response_prefix: missing segment '{seg}'"),
            })?;
        }
    }
    response_map(cur)
}

/// Unified pagination state machine driven by the composable `PaginationConfig`.
/// No style-enum branching — param types and stop conditions carry all information.
pub(crate) struct PaginationLoopState {
    /// Current value for each param. `None` = `FromResponse` not yet received.
    param_values: indexmap::IndexMap<String, Option<serde_json::Value>>,
    /// Next-page absolute URL (`LinkHeader` and `ResponseNextUrl` locations).
    pub(crate) next_absolute_url: Option<String>,
    /// Page size used on the last request (for short-page heuristic).
    pub(crate) last_requested_limit: u32,
    /// BlockRange: current starting block.
    from_block: Option<u64>,
    /// BlockRange: user-specified final block (optional upper bound).
    final_to_block: Option<u64>,
    /// BlockRange: end of last requested range.
    last_requested_to_block: Option<u64>,
}

impl PaginationLoopState {
    pub(crate) fn new(
        pconf: &PaginationConfig,
        user: &QueryPagination,
        consume: &StreamConsumeOpts,
    ) -> Result<Self, RuntimeError> {
        if pconf.location == plasm_compile::PaginationLocation::BlockRange {
            if user.from_block.is_none() {
                return Err(RuntimeError::ConfigurationError {
                    message:
                        "block_range pagination requires QueryPagination.from_block / --from-block"
                            .to_string(),
                });
            }
            if consume.fetch_all && user.to_block.is_none() {
                return Err(RuntimeError::ConfigurationError {
                    message: "block_range pagination with --all requires QueryPagination.to_block / --to-block"
                        .to_string(),
                });
            }
            return Ok(Self {
                param_values: indexmap::IndexMap::new(),
                next_absolute_url: None,
                last_requested_limit: 0,
                from_block: user.from_block,
                final_to_block: user.to_block,
                last_requested_to_block: None,
            });
        }

        let mut param_values = indexmap::IndexMap::new();
        for (name, param) in &pconf.params {
            let initial = match param {
                plasm_compile::PaginationParam::Counter { counter, .. } => {
                    let start = if name == "page" || name == "p" {
                        user.page.unwrap_or(*counter)
                    } else if name == "offset" {
                        user.offset.unwrap_or(*counter)
                    } else {
                        *counter
                    };
                    Some(serde_json::Value::Number(start.into()))
                }
                plasm_compile::PaginationParam::Fixed { fixed } => Some(fixed.clone()),
                plasm_compile::PaginationParam::FromResponse { .. } => user
                    .cursor
                    .as_ref()
                    .map(|c| serde_json::Value::String(c.clone())),
            };
            param_values.insert(name.clone(), initial);
        }

        Ok(Self {
            param_values,
            next_absolute_url: None,
            last_requested_limit: 0,
            from_block: user.from_block,
            final_to_block: user.to_block,
            last_requested_to_block: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_request_params(
        &mut self,
        compiled: &mut CompiledOperation,
        pconf: &PaginationConfig,
        _user: &QueryPagination,
        consume: &StreamConsumeOpts,
        _single_page: bool,
        _is_first_page: bool,
        accumulated: usize,
    ) -> Result<(), RuntimeError> {
        let default_lim = pagination_default_limit(pconf);

        // BlockRange is handled separately.
        if pconf.location == plasm_compile::PaginationLocation::BlockRange {
            let from_block = self
                .from_block
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: "block_range pagination requires a starting block".to_string(),
                })?;
            let span = u64::from(default_lim).max(1);
            let mut to_block = from_block.saturating_add(span.saturating_sub(1));
            if let Some(final_to) = self.final_to_block {
                to_block = to_block.min(final_to);
            }
            match compiled {
                CompiledOperation::Http(_) | CompiledOperation::GraphQl(_) => {
                    compiled_query_insert(
                        compiled,
                        "from_block",
                        Value::String(from_block.to_string()),
                    )?;
                    compiled_query_insert(
                        compiled,
                        "to_block",
                        Value::String(to_block.to_string()),
                    )?;
                }
                CompiledOperation::EvmLogs(_) => {
                    compiled_block_range_set(compiled, from_block, to_block)?;
                }
                CompiledOperation::EvmCall(_) => {
                    return Err(RuntimeError::ConfigurationError {
                        message: "block_range pagination is not valid for evm_call transport"
                            .to_string(),
                    });
                }
                CompiledOperation::View(_) => {
                    return Err(RuntimeError::ConfigurationError {
                        message: "block_range pagination is not valid for composed view transport"
                            .to_string(),
                    });
                }
            }
            self.last_requested_to_block = Some(to_block);
            self.last_requested_limit = default_lim;
            return Ok(());
        }

        // LinkHeader / ResponseNextUrl: no params to inject on absolute-URL continuations;
        // first-page params for ResponseNextUrl are injected via the Query arm below.
        if pconf.location == plasm_compile::PaginationLocation::LinkHeader {
            self.last_requested_limit = default_lim;
            return Ok(());
        }

        let remain_cap = consume
            .max_items
            .map(|c| c.saturating_sub(accumulated))
            .unwrap_or(usize::MAX);
        let limit_this_page: u32 = if remain_cap < usize::MAX {
            (remain_cap as u32).min(default_lim).max(1)
        } else {
            default_lim
        };

        for (name, param) in &pconf.params {
            let current = self.param_values.get(name).and_then(|v| v.as_ref());

            let value = match param {
                plasm_compile::PaginationParam::Fixed { fixed } => {
                    let name_lower = name.to_lowercase();
                    let is_size_like = [
                        "size",
                        "limit",
                        "per_page",
                        "page_size",
                        "maxresults",
                        "max_results",
                        "first",
                    ]
                    .iter()
                    .any(|s| name_lower.contains(s))
                        || name_lower == "first"
                        || name_lower.ends_with("_size")
                        || name_lower.ends_with("_limit");
                    if is_size_like {
                        serde_json::Value::Number(
                            (limit_this_page as i64)
                                .min(fixed.as_i64().unwrap_or(limit_this_page as i64))
                                .into(),
                        )
                    } else {
                        fixed.clone()
                    }
                }
                _ => match current {
                    Some(v) => v.clone(),
                    None => continue, // FromResponse absent on first page — skip
                },
            };

            let plasm_val = json_to_plasm_value(&value);
            match pconf.location {
                plasm_compile::PaginationLocation::Query
                | plasm_compile::PaginationLocation::ResponseNextUrl => {
                    compiled_query_insert(compiled, name, plasm_val)?;
                }
                plasm_compile::PaginationLocation::Body => {
                    use indexmap::IndexMap;
                    if let CompiledOperation::Http(ref mut req)
                    | CompiledOperation::GraphQl(ref mut req) = compiled
                    {
                        if req.body_format == plasm_compile::HttpBodyFormat::Multipart {
                            return Err(RuntimeError::ConfigurationError {
                                message: "pagination with location body is not supported for multipart HTTP requests"
                                    .to_string(),
                            });
                        }
                        if req.body.is_none() {
                            req.body = Some(Value::Object(IndexMap::new()));
                        }
                        if let Some(body) = req.body.as_mut() {
                            merge_pagination_into_body(
                                body,
                                pconf.body_merge_path.as_deref(),
                                name,
                                plasm_val,
                            )?;
                        }
                    }
                }
                _ => {}
            }
        }

        self.last_requested_limit = limit_this_page;
        Ok(())
    }

    pub(crate) fn advance_after_page(
        &mut self,
        pconf: &PaginationConfig,
        response: &serde_json::Value,
        full_page_len: usize,
        requested_limit: u32,
        link_next: Option<&str>,
        _last_entity_id: Option<&str>,
    ) -> Result<bool, RuntimeError> {
        // LinkHeader: next URL from response header.
        if pconf.location == plasm_compile::PaginationLocation::LinkHeader {
            let Some(url) = link_next.filter(|u| !u.is_empty()) else {
                return Ok(false);
            };
            self.next_absolute_url = Some(url.to_string());
            return Ok(true);
        }

        // ResponseNextUrl: next URL from a JSON body field (e.g. Graph @odata.nextLink).
        if pconf.location == plasm_compile::PaginationLocation::ResponseNextUrl {
            let field = pconf
                .response_next_url_field
                .as_deref()
                .unwrap_or("@odata.nextLink");
            let url = if let Some(prefix) = pconf.response_prefix.as_ref().filter(|p| !p.is_empty())
            {
                pagination_context_map(response, Some(prefix.as_slice()))
                    .ok()
                    .and_then(|resp| resp.get(field).and_then(|v| v.as_str()))
            } else {
                response.get(field).and_then(|v| v.as_str())
            };
            let Some(url) = url.filter(|u| !u.is_empty()) else {
                return Ok(false);
            };
            self.next_absolute_url = Some(url.to_string());
            return Ok(true);
        }

        // BlockRange: advance from_block past the last requested range.
        if pconf.location == plasm_compile::PaginationLocation::BlockRange {
            let Some(last_to) = self.last_requested_to_block else {
                return Ok(false);
            };
            if let Some(final_to) = self.final_to_block {
                if last_to >= final_to {
                    return Ok(false);
                }
            }
            self.from_block = Some(last_to.saturating_add(1));
            return Ok(true);
        }

        // Explicit stop_when condition.
        if let Some(stop) = &pconf.stop_when {
            let resp = pagination_context_map(response, pconf.response_prefix.as_deref())?;
            match stop {
                plasm_compile::PaginationStop::FieldEquals { field, eq } => {
                    if let Some(val) = resp.get(field) {
                        if val == eq {
                            return Ok(false);
                        }
                    }
                }
                plasm_compile::PaginationStop::FieldAbsent { field, absent } => {
                    let is_absent = resp.get(field).map(|v| v.is_null()).unwrap_or(true);
                    if is_absent == *absent {
                        return Ok(false);
                    }
                }
            }
        }

        // Update param values for the next request.
        let mut any_from_response_absent = false;
        for (name, param) in &pconf.params {
            match param {
                plasm_compile::PaginationParam::Counter { step, max, .. } => {
                    if let Some(Some(serde_json::Value::Number(n))) = self.param_values.get(name) {
                        let current = n.as_i64().unwrap_or(0);
                        let next = current + step;
                        if max.is_some_and(|m| next > m) {
                            return Ok(false);
                        }
                        self.param_values
                            .insert(name.clone(), Some(serde_json::Value::Number(next.into())));
                    }
                }
                plasm_compile::PaginationParam::FromResponse { from_response } => {
                    let extracted = if let Some(prefix) =
                        pconf.response_prefix.as_ref().filter(|p| !p.is_empty())
                    {
                        pagination_context_map(response, Some(prefix.as_slice()))
                            .ok()
                            .and_then(|resp| {
                                resp.get(from_response.as_str())
                                    .filter(|v| {
                                        !v.is_null()
                                            && v.as_str().map(|s| !s.is_empty()).unwrap_or(true)
                                    })
                                    .cloned()
                            })
                    } else {
                        response
                            .get(from_response.as_str())
                            .filter(|v| {
                                !v.is_null() && v.as_str().map(|s| !s.is_empty()).unwrap_or(true)
                            })
                            .cloned()
                    };
                    if extracted.is_none() {
                        any_from_response_absent = true;
                    }
                    self.param_values.insert(name.clone(), extracted);
                }
                plasm_compile::PaginationParam::Fixed { .. } => {} // fixed, never changes
            }
        }

        // Implicit stop: any FromResponse param became absent → cursor exhausted.
        if any_from_response_absent && pconf.stop_when.is_none() {
            return Ok(false);
        }

        // Default short-page heuristic: stop when items array is shorter than requested.
        if full_page_len == 0 || (full_page_len as u32) < requested_limit {
            return Ok(false);
        }

        Ok(true)
    }
}

impl From<&PaginationLoopState> for QueryPaginationState {
    fn from(s: &PaginationLoopState) -> Self {
        Self {
            param_values: s
                .param_values
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            next_absolute_url: s.next_absolute_url.clone(),
            last_requested_limit: s.last_requested_limit,
            from_block: s.from_block,
            final_to_block: s.final_to_block,
            last_requested_to_block: s.last_requested_to_block,
        }
    }
}

impl TryFrom<QueryPaginationState> for PaginationLoopState {
    type Error = RuntimeError;

    fn try_from(s: QueryPaginationState) -> Result<Self, Self::Error> {
        Ok(Self {
            param_values: s.param_values.into_iter().collect(),
            next_absolute_url: s.next_absolute_url,
            last_requested_limit: s.last_requested_limit,
            from_block: s.from_block,
            final_to_block: s.final_to_block,
            last_requested_to_block: s.last_requested_to_block,
        })
    }
}
