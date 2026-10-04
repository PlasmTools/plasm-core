//! Canonical pagination contract validation (compile/load + runtime).

use crate::cml::{PaginationConfig, PaginationLocation, PaginationParam, PaginationStrategyKind};

/// Validated pagination block: strategy is present, page-size role is unambiguous.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedPagination {
    config: PaginationConfig,
    strategy: PaginationStrategyKind,
    page_size: Option<u32>,
}

impl ValidatedPagination {
    #[must_use]
    pub fn config(&self) -> &PaginationConfig {
        &self.config
    }

    #[must_use]
    pub fn strategy(&self) -> PaginationStrategyKind {
        self.strategy
    }

    #[must_use]
    pub fn page_size(&self) -> Option<u32> {
        self.page_size
    }
}

/// Fail-closed pagination contract errors (one or more messages).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaginationContractError {
    messages: Vec<String>,
}

impl PaginationContractError {
    #[must_use]
    pub fn messages(&self) -> &[String] {
        &self.messages
    }
}

impl std::fmt::Display for PaginationContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.messages.join("; "))
    }
}

impl std::error::Error for PaginationContractError {}

impl PaginationConfig {
    /// Validate tagged strategy + page-size role. Does not infer wire names.
    pub fn validate(&self) -> Result<ValidatedPagination, PaginationContractError> {
        let mut messages = Vec::new();
        let Some(strategy) = self.strategy else {
            return Err(PaginationContractError {
                messages: vec![
                    "pagination block requires explicit `strategy:` (page_number|offset|cursor|next_url|link_header|block_range)"
                        .to_string(),
                ],
            });
        };

        if !self.initial_only_query_params.is_empty() {
            if self.location != PaginationLocation::Query
                || matches!(
                    strategy,
                    PaginationStrategyKind::NextUrl
                        | PaginationStrategyKind::LinkHeader
                        | PaginationStrategyKind::BlockRange
                )
            {
                messages.push(
                    "initial_only_query_params requires query-parameter pagination without an absolute-URL continuation"
                        .to_string(),
                );
            }
            let mut seen = std::collections::HashSet::new();
            for name in &self.initial_only_query_params {
                if name.is_empty() || !seen.insert(name) {
                    messages.push(format!(
                        "initial_only_query_params contains an empty or duplicate key `{name}`"
                    ));
                }
                if self.params.contains_key(name) {
                    messages.push(format!(
                        "initial-only query key `{name}` must not also be a pagination parameter"
                    ));
                }
            }
        }

        let size_params: Vec<(&str, &PaginationParam)> = self
            .params
            .iter()
            .filter(|(_, p)| p.is_page_size_role())
            .map(|(n, p)| (n.as_str(), p))
            .collect();
        if size_params.len() > 1 {
            messages.push(
                "pagination block must declare at most one Fixed param with `role: page_size`"
                    .to_string(),
            );
        }
        let page_size = size_params
            .first()
            .and_then(|(_, p)| p.fixed_as_u32())
            .filter(|&n| n > 0);
        if size_params
            .first()
            .is_some_and(|(_, p)| p.fixed_as_u32().unwrap_or(0) == 0)
        {
            messages.push("page_size role must be a positive integer".to_string());
        }
        if requires_page_size_role(strategy, self) && size_params.is_empty() {
            messages.push(format!(
                "pagination strategy `{strategy}` requires exactly one Fixed param with `role: page_size`"
            ));
        }

        if matches!(strategy, PaginationStrategyKind::Offset) {
            if let Some(size) = page_size {
                for (name, param) in &self.params {
                    if let PaginationParam::Counter { step, .. } = param {
                        if *step != i64::from(size) {
                            messages.push(format!(
                                "offset strategy counter `{name}` step ({step}) must equal page_size ({size})"
                            ));
                        }
                    }
                }
            }
        }

        for (name, param) in &self.params {
            if let PaginationParam::Counter { step, .. } = param {
                if *step <= 0 {
                    messages.push(format!("pagination counter `{name}` step must be positive"));
                }
            }
        }

        if !messages.is_empty() {
            return Err(PaginationContractError { messages });
        }
        Ok(ValidatedPagination {
            config: self.clone(),
            strategy,
            page_size,
        })
    }
}

fn requires_page_size_role(strategy: PaginationStrategyKind, config: &PaginationConfig) -> bool {
    match strategy {
        PaginationStrategyKind::PageNumber
        | PaginationStrategyKind::Offset
        | PaginationStrategyKind::BlockRange => true,
        PaginationStrategyKind::Cursor
        | PaginationStrategyKind::NextUrl
        | PaginationStrategyKind::LinkHeader => {
            config.stop_when.is_none()
                && !config
                    .params
                    .values()
                    .any(|p| matches!(p, PaginationParam::FromResponse { .. }))
                && !matches!(
                    config.location,
                    PaginationLocation::LinkHeader | PaginationLocation::ResponseNextUrl
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cml::{PaginationLocation, PaginationParam, PaginationParamRole};

    #[test]
    fn rejects_missing_strategy() {
        let cfg = PaginationConfig {
            strategy: None,
            params: indexmap::indexmap! {},
            location: PaginationLocation::Query,
            body_merge_path: None,
            response_prefix: None,
            response_next_url_field: None,
            initial_only_query_params: vec![],
            stop_when: None,
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn offset_counter_must_match_page_size() {
        let cfg = PaginationConfig {
            strategy: Some(PaginationStrategyKind::Offset),
            params: indexmap::indexmap! {
                "offset".into() => PaginationParam::Counter { counter: 0, step: 1, max: None },
                "limit".into() => PaginationParam::Fixed {
                    fixed: serde_json::json!(20),
                    role: Some(PaginationParamRole::PageSize),
                },
            },
            location: PaginationLocation::Query,
            body_merge_path: None,
            response_prefix: None,
            response_next_url_field: None,
            initial_only_query_params: vec![],
            stop_when: None,
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn initial_only_query_keys_are_distinct_from_driver_params_and_require_query_paging() {
        let mut cfg = PaginationConfig {
            strategy: Some(PaginationStrategyKind::PageNumber),
            params: indexmap::indexmap! {
                "page_index".into() => PaginationParam::Counter { counter: 0, step: 1, max: None },
                "page_limit".into() => PaginationParam::Fixed {
                    fixed: serde_json::json!(20),
                    role: Some(PaginationParamRole::PageSize),
                },
            },
            location: PaginationLocation::Query,
            body_merge_path: None,
            response_prefix: None,
            response_next_url_field: None,
            initial_only_query_params: vec!["include_self".into()],
            stop_when: None,
        };
        cfg.validate().expect("valid initial-only query field");
        cfg.initial_only_query_params.push("include_self".into());
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
        cfg.initial_only_query_params = vec!["page_index".into()];
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("must not also"));
        cfg.initial_only_query_params = vec!["include_self".into()];
        cfg.location = PaginationLocation::Body;
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("requires query-parameter"));
    }
}
