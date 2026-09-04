//! Static warnings/validation for list capabilities' CML pagination contracts.

use plasm_cml::{
    parse_capability_template, template_pagination, template_var_names, CapabilityTemplate,
    HttpMethod,
};
use plasm_core::{CapabilityKind, CapabilitySchema, CGS};
use std::collections::BTreeSet;
use tracing::warn;

const PAGINATION_WIRE_NAMES: &[&str] = &[
    "page_index",
    "page_limit",
    "page",
    "offset",
    "limit",
    "cursor",
    "after",
    "before",
    "startAt",
    "maxResults",
    "max_results",
    "per_page",
    "page_size",
    "skip",
    "take",
    "first",
];

/// Names treated as vendor pagination knobs when declared on query/search capabilities.
#[must_use]
pub fn is_pagination_wire_param(name: &str) -> bool {
    PAGINATION_WIRE_NAMES.contains(&name)
        || name.ends_with("_cursor")
        || name.ends_with("_page_token")
        || name == "next_page_token"
}

/// Capabilities whose CML exposes pagination wire params but omits a composable `pagination:` block.
///
/// Runtime behavior without the block is a **single upstream HTTP page** (no default `fetch_all`).
#[must_use]
pub fn paginated_list_missing_cml_pagination_warnings(cgs: &CGS) -> Vec<String> {
    let mut warnings = Vec::new();
    for (name, cap) in &cgs.capabilities {
        if let Some(msg) = missing_pagination_warning(name, cap) {
            warnings.push(msg);
        }
    }
    warnings.sort_unstable();
    warnings
}

/// Invalid pagination contracts (missing strategy, missing page_size role, bad offset step).
#[must_use]
pub fn pagination_contract_validation_errors(cgs: &CGS) -> Vec<String> {
    let mut errors = Vec::new();
    for (name, cap) in &cgs.capabilities {
        if let Some(msgs) = validate_capability_pagination(name, cap) {
            errors.extend(msgs);
        }
    }
    errors.sort_unstable();
    errors
}

/// Log pagination warnings/errors at `warn` level (catalog load / pack).
pub fn emit_paginated_list_missing_cml_pagination_warnings(cgs: &CGS) {
    for msg in paginated_list_missing_cml_pagination_warnings(cgs) {
        warn!(
            target: "plasm_compile::capability_pagination",
            violation = %msg,
            "paginated list missing CML pagination"
        );
    }
    for msg in pagination_contract_validation_errors(cgs) {
        warn!(
            target: "plasm_compile::capability_pagination",
            violation = %msg,
            "pagination contract invalid"
        );
    }
}

fn missing_pagination_warning(name: &str, cap: &CapabilitySchema) -> Option<String> {
    if !matches!(cap.kind, CapabilityKind::Query | CapabilityKind::Search) {
        return None;
    }
    let mapping = cap.mapping.as_ref()?;
    let template = parse_capability_template(&mapping.template.0).ok()?;
    if matches!(template, CapabilityTemplate::View(_)) {
        return None;
    }
    let is_http_get = matches!(
        &template,
        CapabilityTemplate::Http(req) if req.method == HttpMethod::Get
    );
    if !is_http_get {
        return None;
    }
    if template_pagination(&template).is_some() {
        return None;
    }

    let domain_params: BTreeSet<String> = cap
        .input_fields()
        .filter(|f| is_pagination_wire_param(f.name.as_str()))
        .map(|f| f.name.to_string())
        .collect();
    let cml_params: BTreeSet<String> = template_var_names(&template)
        .into_iter()
        .filter(|n| is_pagination_wire_param(n))
        .collect();
    if domain_params.is_empty() && cml_params.is_empty() {
        return None;
    }

    let params: Vec<String> = domain_params.union(&cml_params).cloned().collect();
    let source = match (!domain_params.is_empty(), !cml_params.is_empty()) {
        (true, true) => "domain.yaml and CML",
        (true, false) => "domain.yaml",
        (false, true) => "CML",
        (false, false) => unreachable!(),
    };
    Some(format!(
        "capability `{name}`: {source} declares pagination wire param(s) [{}] but mappings.yaml has no composable `pagination:` block — runtime performs a single upstream HTTP page (no default fetch-all); add `pagination:` (see plasm-authoring/reference.md#pagination-cml--mappingsyaml-only)",
        params.join(", ")
    ))
}

fn validate_capability_pagination(name: &str, cap: &CapabilitySchema) -> Option<Vec<String>> {
    let mapping = cap.mapping.as_ref()?;
    let template = parse_capability_template(&mapping.template.0).ok()?;
    let pconf = template_pagination(&template)?;
    match pconf.validate() {
        Ok(_) => None,
        Err(err) => Some(
            err.messages()
                .iter()
                .map(|m| format!("capability `{name}`: {m}"))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn load_schema(rel: &str) -> CGS {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        plasm_core::load_schema_dir(&root.join(rel)).expect("load schema")
    }

    #[test]
    fn pokeapi_mini_with_pagination_block_has_no_warnings() {
        let cgs = load_schema("../../fixtures/schemas/pokeapi_mini");
        assert!(
            paginated_list_missing_cml_pagination_warnings(&cgs).is_empty(),
            "expected no pagination warnings"
        );
        assert!(
            pagination_contract_validation_errors(&cgs).is_empty(),
            "expected valid pagination contract: {:?}",
            pagination_contract_validation_errors(&cgs)
        );
    }

    #[test]
    fn appworld_gmail_list_caps_have_pagination_blocks() {
        let cgs = load_schema("../../apis/appworld/gmail");
        let warnings = paginated_list_missing_cml_pagination_warnings(&cgs);
        assert!(
            warnings.is_empty(),
            "gmail list caps must use composable pagination:: {warnings:?}"
        );
        let errs = pagination_contract_validation_errors(&cgs);
        assert!(
            errs.is_empty(),
            "gmail page_number contracts must validate: {errs:?}"
        );
    }

    #[test]
    fn appworld_amazon_list_caps_have_pagination_blocks() {
        let cgs = load_schema("../../apis/appworld/amazon");
        let warnings = paginated_list_missing_cml_pagination_warnings(&cgs);
        assert!(
            warnings.is_empty(),
            "amazon list caps must use composable pagination:: {warnings:?}"
        );
        let errs = pagination_contract_validation_errors(&cgs);
        assert!(
            errs.is_empty(),
            "amazon page_number contracts must validate: {errs:?}"
        );
    }

    #[test]
    fn appworld_venmo_pagination_contract_is_valid() {
        let cgs = load_schema("../../apis/appworld/venmo");
        let errs = pagination_contract_validation_errors(&cgs);
        assert!(
            errs.is_empty(),
            "venmo page_number contracts must validate: {errs:?}"
        );
    }
}
