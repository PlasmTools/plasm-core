//! Nominal narrowing selects existing catalog occurrences, never constructs receivers.
use super::super::schema_validate;
use super::super::types::DagNodeSource;
use super::*;

impl Lower<'_> {
    pub(super) fn narrow_entity(
        &mut self,
        site: &PyExpr,
        call: &ruff_python_ast::ExprCall,
        source: &str,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        let [PyExpr::Name(target)] = call.arguments.args.as_ref() else {
            return Err(at(site, PythonSourceError::EntityNarrowingTarget));
        };
        if !call.arguments.keywords.is_empty() {
            return Err(at(site, PythonSourceError::EntityNarrowingTarget));
        }
        let target = self
            .state
            .sym_map_for(self.es)
            .resolve_session_entity(target.id.as_str())
            .map_err(|_| at(site, PythonSourceError::EntityNarrowingTarget))?;
        let contract = super::super::binding_contract(&self.state, source)
            .ok_or_else(|| at(site, PythonSourceError::EntityNarrowingSource))?;
        if !matches!(
            contract.row_entity,
            crate::program_binding::RowEntityAuthority::Union(_)
        ) {
            return Err(at(site, PythonSourceError::EntityNarrowingSource));
        }
        let target = QualifiedEntityKey {
            entry_id: target.entry_id.to_string(),
            entity: target.entity.to_string(),
        };
        if !contract.row_entity.contains(&target) {
            return Err(at(site, PythonSourceError::EntityNarrowingTarget));
        }
        let schema = schema_validate::compute_schema::synthetic_schema_for_qualified_entity(
            self.es, &target,
        )
        .map_err(super::super::error::DagCompilationError::from)?;
        self.insert(DagNode {
            id: id.into(),
            expr: String::new(),
            singleton: false,
            page_size: None,
            source: DagNodeSource::Compute {
                source: source.into(),
                op: ComputeOp::NarrowEntity {
                    source_at_most_one: contract.row_cardinality.permits_identity_receiver(),
                    target: PlanQualifiedEntityKey {
                        entry_id: target.entry_id,
                        entity: target.entity,
                    },
                },
                schema,
                collection_alias: None,
            },
        })
    }
}
