//! Inline structural invoke examples for union constructor bodies.

use crate::symbol_tuning::SymbolMap;
use crate::CGS;

use super::super::teaching_util::TEACHING_PARAM_VALUE_PLACEHOLDER;

/// LHS teaching token for a capability param path inside union-constructor `{…}` bodies.
/// Nested expand paths use the wire **leaf** (`ref`, `markdown`, …), not the full dotted path.
#[inline]
fn cap_param_structural_lhs(
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    domain: &str,
    cap_name: &str,
    full_path: &str,
    field_name: &str,
) -> String {
    map.map(|m| m.teaching_slot_token_cap_param(catalog_entry_id, domain, cap_name, full_path))
        .unwrap_or_else(|| field_name.to_string())
}

/// Structural invoke RHS inside union constructors (`v101{…}`): wire leaf names when a
/// [`SymbolMap`] is present (teaching TSV); canonical [`RenderMode`] uses wire names.
pub(crate) fn format_inline_structural_example_symbolic(
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    domain: &str,
    cap_name: &str,
    path_prefix: &str,
    ty: &crate::InputType,
    _cgs: &CGS,
) -> String {
    match ty {
        crate::InputType::None | crate::InputType::Value { .. } => {
            TEACHING_PARAM_VALUE_PLACEHOLDER.to_string()
        }
        crate::InputType::Object { fields, .. } => {
            let mut parts = Vec::new();
            for sf in fields {
                let seg = if path_prefix.is_empty() {
                    sf.name.clone()
                } else {
                    format!("{path_prefix}.{}", sf.name)
                };
                match &sf.wire {
                    crate::InputFieldWire::Inline(inner) => {
                        let rhs = format_inline_structural_example_symbolic(
                            map,
                            catalog_entry_id,
                            domain,
                            cap_name,
                            &seg,
                            inner.as_ref(),
                            _cgs,
                        );
                        let lhs = cap_param_structural_lhs(
                            map,
                            catalog_entry_id,
                            domain,
                            cap_name,
                            seg.as_str(),
                            sf.name.as_str(),
                        );
                        parts.push(format!("{lhs}={rhs}"));
                    }
                    crate::InputFieldWire::Registry(_) => {
                        let lhs = cap_param_structural_lhs(
                            map,
                            catalog_entry_id,
                            domain,
                            cap_name,
                            seg.as_str(),
                            sf.name.as_str(),
                        );
                        parts.push(format!("{lhs}={}", TEACHING_PARAM_VALUE_PLACEHOLDER));
                    }
                }
            }
            format!("{{{}}}", parts.join(","))
        }
        crate::InputType::Array { element_type, .. } => {
            format!(
                "[{}]",
                format_inline_structural_example_symbolic(
                    map,
                    catalog_entry_id,
                    domain,
                    cap_name,
                    path_prefix,
                    element_type.as_ref(),
                    _cgs,
                )
            )
        }
        crate::InputType::Union { .. } => TEACHING_PARAM_VALUE_PLACEHOLDER.to_string(),
    }
}

/// Like [`format_inline_structural_example_symbolic`] for an object body, but **only required** fields
/// and **no** `,..` optional tail — union constructor payloads inside `{…}` must parse as plain `k=v` pairs.
pub(crate) fn format_inline_structural_example_symbolic_required_only(
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    domain: &str,
    cap_name: &str,
    path_prefix: &str,
    ty: &crate::InputType,
    cgs: &CGS,
) -> String {
    let crate::InputType::Object { fields, .. } = ty else {
        return format_inline_structural_example_symbolic(
            map,
            catalog_entry_id,
            domain,
            cap_name,
            path_prefix,
            ty,
            cgs,
        );
    };
    let mut parts = Vec::new();
    for sf in fields {
        if !sf.required {
            continue;
        }
        let seg = if path_prefix.is_empty() {
            sf.name.clone()
        } else {
            format!("{path_prefix}.{}", sf.name)
        };
        match &sf.wire {
            crate::InputFieldWire::Inline(inner) => {
                let rhs = format_inline_structural_example_symbolic_required_only(
                    map,
                    catalog_entry_id,
                    domain,
                    cap_name,
                    &seg,
                    inner.as_ref(),
                    cgs,
                );
                let lhs = cap_param_structural_lhs(
                    map,
                    catalog_entry_id,
                    domain,
                    cap_name,
                    seg.as_str(),
                    sf.name.as_str(),
                );
                parts.push(format!("{lhs}={rhs}"));
            }
            crate::InputFieldWire::Registry(_) => {
                let lhs = cap_param_structural_lhs(
                    map,
                    catalog_entry_id,
                    domain,
                    cap_name,
                    seg.as_str(),
                    sf.name.as_str(),
                );
                parts.push(format!("{lhs}={}", TEACHING_PARAM_VALUE_PLACEHOLDER));
            }
        }
    }
    let inner = parts.join(",");
    format!("{{{inner}}}")
}
