//! Surface [`QueryExpr`] → canonical [`ResolvedRowset`] (read-query seam).
//!
//! Lane discipline (RA-1 / RA-2):
//! - Source braces bind catalog selection and root scope pivots → [`BackendSelection`].
//! - Capability control params in braces are rejected (RA-1); pagination/hydrate use
//!   [`InvocationControls`].
//! - Entity-row fields in braces are rejected (RA-2); use `.filter` for row predicates.

use std::collections::HashSet;

use crate::cgs_federation::QualifiedEntityKey;
use crate::expr::QueryExpr;
use crate::identity::{CapabilityName, CapabilityParamName};
use crate::plasm_monad::FieldPath;
use crate::query_resolve::resolve_query_capability;
use crate::schema::CGS;
use crate::{CompOp, Predicate, TypedComparisonValue};

use super::{
    BackendSelection, BackendSelectionBinding, InvocationControls, ParentScope, ResolvedRowset,
    RowSource, RowTerminal,
};

/// Normalize a surface read query into a lane-typed [`ResolvedRowset`].
///
/// `entry_id` stamps [`QualifiedEntityKey`]; prefer the query's catalog stamp when present
/// (callers typically pass `query.catalog_entry_id.or(cgs.entry_id).unwrap_or("")`).
pub fn normalize_query_expr_to_rowset(
    query: &QueryExpr,
    cgs: &CGS,
    entry_id: &str,
) -> Result<ResolvedRowset, String> {
    let cap = resolve_query_capability(query, cgs).map_err(|e| e.to_string())?;

    let selection_names: HashSet<&str> = cap
        .selection_params()
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    let scope_names: HashSet<&str> = cap.scope_params().iter().map(|f| f.name.as_str()).collect();
    let control_names: HashSet<&str> = cap
        .control_params()
        .iter()
        .map(|f| f.name.as_str())
        .collect();

    // Root source braces bind wire slots only. Catalog `inputs.selection` and root
    // `inputs.scope` pivots both become [`BackendSelection`] on `ParentScope::Root`
    // (relation hops alone use [`ParentScope::Relation`]; RA-6). Capability
    // `inputs.controls` and entity-row fields never enter braces (RA-1 / RA-2).
    let selection = match &query.predicate {
        None => BackendSelection::default(),
        Some(pred) => {
            let comparisons = collect_source_comparisons(pred)?;
            let mut bindings = Vec::with_capacity(comparisons.len());
            for (field, op, value) in comparisons {
                if control_names.contains(field.as_str()) {
                    return Err(format!(
                        "RA-1: '{field}' is an invocation-control slot; source braces accept backend-selection (and root scope pivots) only — use pagination/hydrate controls, not braces"
                    ));
                }
                if selection_names.contains(field.as_str()) || scope_names.contains(field.as_str())
                {
                    bindings.push(BackendSelectionBinding {
                        slot: CapabilityParamName::from(field.as_str()),
                        op,
                        value,
                    });
                    continue;
                }
                return Err(format!(
                    "RA-2: '{field}' is not a declared selection/scope parameter of capability '{}'; source braces bind backend-selection slots only (row predicates use `| where`)",
                    cap.name
                ));
            }
            BackendSelection(bindings)
        }
    };

    let projection = match &query.projection {
        None => Vec::new(),
        Some(fields) => {
            let mut out = Vec::with_capacity(fields.len());
            for f in fields {
                out.push(
                    FieldPath::new(vec![f.clone()])
                        .map_err(|e| format!("projection field '{f}': {e}"))?,
                );
            }
            out
        }
    };

    let effective_entry = query
        .catalog_entry_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(entry_id);

    Ok(ResolvedRowset {
        source: RowSource::External {
            qualified_entity: QualifiedEntityKey::new(effective_entry, query.entity.clone()),
            capability: CapabilityName::from(cap.name.as_str()),
            parent_scope: ParentScope::Root,
            controls: InvocationControls {
                pagination: query.pagination.clone(),
                hydrate: query.hydrate,
            },
        },
        selection,
        transforms: Vec::new(),
        projection,
        terminal: RowTerminal::Rows,
    })
}

fn collect_source_comparisons(
    pred: &Predicate,
) -> Result<Vec<(String, CompOp, TypedComparisonValue)>, String> {
    match pred {
        Predicate::True => Ok(Vec::new()),
        Predicate::Comparison { field, op, value } => {
            Ok(vec![(field.clone(), *op, value.clone())])
        }
        Predicate::And { args } => {
            let mut out = Vec::new();
            for arg in args {
                out.extend(collect_source_comparisons(arg)?);
            }
            Ok(out)
        }
        Predicate::False => Err(
            "source braces do not accept a false predicate; omit braces or use selection comparisons"
                .into(),
        ),
        Predicate::Or { .. } => Err(
            "source braces do not accept OR; backend selection is a flat conjunction of selection slots"
                .into(),
        ),
        Predicate::Not { .. } => Err(
            "source braces do not accept NOT; backend selection is a flat conjunction of selection slots"
                .into(),
        ),
        Predicate::ExistsRelation { .. } => Err(
            "RA-1: exists_relation belongs on materialized rows (.filter), not source braces"
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        registry_test_util, BackendSelectionSchema, CapabilityInputs, CapabilityKind,
        CapabilityMapping, CapabilitySchema, NamedValueSchema, ParentScopeSchema, ResourceSchema,
    };
    use crate::{FieldType, InvocationControlsSchema, QueryPagination};

    fn seed_values(cgs: &mut CGS) {
        cgs.values.insert(
            "fx_str".into(),
            NamedValueSchema {
                domain: Default::default(),
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                array_items: None,
                currency: None,
            },
        );
    }

    fn fixture_cgs(with_selection: bool) -> CGS {
        let mut cgs = CGS::new();
        seed_values(&mut cgs);
        cgs.bind_registry_entry_id("app");

        cgs.add_resource(ResourceSchema {
            name: "Request".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                registry_test_util::entity_field_from_values(&cgs, "fx_str", "id", true, ""),
                registry_test_util::entity_field_from_values(&cgs, "fx_str", "status", false, ""),
                registry_test_util::entity_field_from_values(&cgs, "fx_str", "name", false, ""),
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .unwrap();

        let path = vec![serde_json::json!({"type": "literal", "value": "requests"})];

        let selection = if with_selection {
            BackendSelectionSchema(vec![registry_test_util::object_input_field_from_values(
                &cgs, "fx_str", "status", false,
            )])
        } else {
            BackendSelectionSchema::default()
        };

        let scope = ParentScopeSchema(vec![registry_test_util::object_input_field_from_values(
            &cgs,
            "fx_str",
            "parent_id",
            false,
        )]);

        let controls =
            InvocationControlsSchema(vec![registry_test_util::object_input_field_from_values(
                &cgs,
                "fx_str",
                "page_token",
                false,
            )]);

        cgs.add_capability(CapabilitySchema {
            name: "request_query".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Request".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": path
                })
                .into(),
            }),
            derived: None,
            inputs: CapabilityInputs {
                scope,
                selection,
                controls,
                ..CapabilityInputs::default()
            },
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],
            deterministic: None,
        })
        .unwrap();

        cgs.add_capability(CapabilitySchema {
            name: "request_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Request".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [
                        {"type": "literal", "value": "requests"},
                        {"type": "var", "name": "id"}
                    ]
                })
                .into(),
            }),
            derived: None,
            inputs: CapabilityInputs::default(),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],
            deterministic: None,
        })
        .unwrap();

        cgs.validate().expect("fixture CGS must validate");
        cgs
    }

    #[test]
    fn selection_braces_bind_backend_slots() {
        let cgs = fixture_cgs(true);
        let mut q = QueryExpr::filtered("Request", Predicate::eq("status", "open"));
        q.capability_name = Some("request_query".into());

        let rowset = normalize_query_expr_to_rowset(&q, &cgs, "app").expect("normalize");
        assert_eq!(rowset.selection.0.len(), 1);
        assert_eq!(rowset.selection.0[0].slot.as_str(), "status");
    }

    #[test]
    fn selection_only_braces_reject_entity_field() {
        let cgs = fixture_cgs(true);
        let mut q = QueryExpr::filtered("Request", Predicate::eq("name", "x"));
        q.capability_name = Some("request_query".into());

        let err = normalize_query_expr_to_rowset(&q, &cgs, "app").unwrap_err();
        assert!(err.contains("RA-2") || err.contains("selection"), "{err}");
        assert!(err.contains("name"), "{err}");
    }

    #[test]
    fn ra1_root_scope_pivot_becomes_selection_not_parent_scope() {
        let cgs = fixture_cgs(true);
        let mut q = QueryExpr::filtered("Request", Predicate::eq("parent_id", "p1"));
        q.capability_name = Some("request_query".into());

        let rowset = normalize_query_expr_to_rowset(&q, &cgs, "app").expect("normalize");
        match &rowset.source {
            RowSource::External { parent_scope, .. } => {
                assert_eq!(*parent_scope, ParentScope::Root);
            }
            other => panic!("expected External source, got {other:?}"),
        }
        assert_eq!(rowset.selection.0.len(), 1);
        assert_eq!(rowset.selection.0[0].slot.as_str(), "parent_id");
    }

    #[test]
    fn ra1_rejects_control_param_in_braces() {
        let cgs = fixture_cgs(true);
        let mut q = QueryExpr::filtered("Request", Predicate::eq("page_token", "abc"));
        q.capability_name = Some("request_query".into());

        let err = normalize_query_expr_to_rowset(&q, &cgs, "app").unwrap_err();
        assert!(err.contains("RA-1"), "{err}");
        assert!(
            err.contains("control") || err.contains("page_token"),
            "{err}"
        );
    }

    #[test]
    fn ra2_pagination_and_hydrate_land_in_invocation_controls() {
        let cgs = fixture_cgs(true);
        let mut q = QueryExpr::filtered("Request", Predicate::eq("status", "open"));
        q.capability_name = Some("request_query".into());
        q.pagination = Some(QueryPagination {
            page: Some(2),
            ..QueryPagination::default()
        });
        q.hydrate = Some(false);

        let rowset = normalize_query_expr_to_rowset(&q, &cgs, "app").unwrap();
        match &rowset.source {
            RowSource::External { controls, .. } => {
                assert_eq!(controls.hydrate, Some(false));
                assert_eq!(controls.pagination.as_ref().and_then(|p| p.page), Some(2));
            }
            other => panic!("expected External source, got {other:?}"),
        }
        assert!(rowset.selection.0.iter().all(|b| b.slot.as_str() != "page"));
    }
}
