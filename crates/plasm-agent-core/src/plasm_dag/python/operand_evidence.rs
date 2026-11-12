//! Admission-owned meanings of source operands. Unknown values are never example defaults.
use super::*;
use plasm_core::schema::{CapabilitySchema, InputFieldSchema};
use plasm_core::value_domain::KernelKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixedOperand {
    FieldSelection,
    Cardinality,
    InvocationControl,
    Discriminator,
    DomainConstraint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputRole {
    pub entry: plasm_core::identity::RegistryEntryId,
    pub entity: plasm_core::EntityName,
    pub capability: plasm_core::CapabilityName,
    pub parameter: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataOperand {
    EntityIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OperandMeaning {
    Data { role: InputRole, kind: DataOperand },
    Fixed(FixedOperand),
    Sensitive,
    Unclassified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperandDisclosure {
    NotSensitive,
    EmbeddedLiteral,
    RuntimeData,
    UnresolvedSensitiveDataflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdmittedOperand {
    pub range: ruff_text_size::TextRange,
    pub meaning: OperandMeaning,
    pub disclosure: OperandDisclosure,
}

pub(crate) fn input_meaning(
    owner: &plasm_core::symbol_tuning::EntityBinding,
    cap: &CapabilitySchema,
    field: &InputFieldSchema,
    cgs: &plasm_core::CGS,
) -> OperandMeaning {
    if field.sink_class.is_some() {
        // A classified sink is not proof that retaining its input is safe.
        return OperandMeaning::Sensitive;
    }
    if cap
        .inputs
        .controls
        .0
        .iter()
        .any(|control| control.name == field.name)
    {
        return OperandMeaning::Fixed(FixedOperand::InvocationControl);
    }
    let value = match &field.wire {
        plasm_core::InputFieldWire::Registry(_) => match field.named_value(cgs) {
            Ok(value) => Some(value),
            Err(_) => return OperandMeaning::Unclassified,
        },
        plasm_core::InputFieldWire::Inline(_) => None,
    };
    if value.is_some_and(|value| {
        value.domain.enum_membership.is_some() || value.allowed_values.is_some()
    }) {
        return OperandMeaning::Fixed(FixedOperand::DomainConstraint);
    }
    let Some(value) = value else {
        return OperandMeaning::Unclassified;
    };
    if matches!(value.domain.kernel, KernelKind::EntityRef { .. }) {
        return OperandMeaning::Data {
            role: InputRole {
                entry: owner.entry_id.clone(),
                entity: owner.entity.clone(),
                capability: cap.name.clone(),
                parameter: field.name.clone(),
            },
            kind: DataOperand::EntityIdentity,
        };
    }
    // Primitive shape and authored prose cannot prove whether a value is a
    // recipient, provider selector, secret, threshold, currency, or ordinary data.
    OperandMeaning::Unclassified
}

impl Lower<'_> {
    pub(super) fn record_operand(&mut self, expression: &PyExpr, meaning: OperandMeaning) {
        let disclosure = if meaning == OperandMeaning::Sensitive {
            self.sensitive_expression_origin(expression)
        } else {
            OperandDisclosure::NotSensitive
        };
        self.admitted_operands.push(AdmittedOperand {
            range: expression.range(),
            meaning,
            disclosure,
        });
    }

    fn sensitive_expression_origin(&self, expression: &PyExpr) -> OperandDisclosure {
        match expression {
            PyExpr::Name(name) => self
                .sensitive_node_origin(self.scoped_binding(name.id.as_str()), &mut BTreeSet::new()),
            PyExpr::Attribute(attr) => self.sensitive_expression_origin(&attr.value),
            _ if super::literal_operands::LiteralOperand::classify(expression).is_some() => {
                OperandDisclosure::EmbeddedLiteral
            }
            _ => OperandDisclosure::UnresolvedSensitiveDataflow,
        }
    }
    fn sensitive_node_origin(
        &self,
        binding: &str,
        visited: &mut BTreeSet<String>,
    ) -> OperandDisclosure {
        use super::super::types::DagNodeSource;
        if !visited.insert(binding.to_owned()) {
            return OperandDisclosure::UnresolvedSensitiveDataflow;
        }
        let Some(node) = self.state.get(binding) else {
            return OperandDisclosure::UnresolvedSensitiveDataflow;
        };
        match &node.source {
            DagNodeSource::Surface { .. } | DagNodeSource::RelationTraversal { .. } => {
                OperandDisclosure::RuntimeData
            }
            DagNodeSource::ScalarExtract { source, .. } => {
                self.sensitive_node_origin(source, visited)
            }
            DagNodeSource::Compute {
                source,
                op:
                    ComputeOp::RequireOne
                    | ComputeOp::Limit { .. }
                    | ComputeOp::Project { .. }
                    | ComputeOp::Filter { .. }
                    | ComputeOp::Sort { .. }
                    | ComputeOp::DedupeBy { .. }
                    | ComputeOp::NarrowEntity { .. },
                ..
            } => self.sensitive_node_origin(source, visited),
            DagNodeSource::Data(value) | DagNodeSource::Derive { value, .. } => {
                self.sensitive_value_origin(value, visited)
            }
            _ => OperandDisclosure::UnresolvedSensitiveDataflow,
        }
    }
    fn sensitive_value_origin(
        &self,
        value: &PlasmDataValue,
        visited: &mut BTreeSet<String>,
    ) -> OperandDisclosure {
        match value {
            PlasmDataValue::Literal { .. } => OperandDisclosure::EmbeddedLiteral,
            PlasmDataValue::NodeSymbol { node, .. } => self.sensitive_node_origin(node, visited),
            PlasmDataValue::EntityRefKey { key, .. } => self.sensitive_value_origin(key, visited),
            _ => OperandDisclosure::UnresolvedSensitiveDataflow,
        }
    }
    fn confidential_field(&self, owner: &QualifiedEntityKey, wire: &str) -> bool {
        self.es
            .contexts_by_entry
            .get(&owner.entry_id)
            .is_some_and(|context| {
                context
                    .cgs
                    .get_entity(&owner.entity)
                    .and_then(|entity| entity.fields.get(wire))
                    .and_then(|field| field.data_class.as_ref())
                    .and_then(|class| context.cgs.data_classes.get(class))
                    .is_some_and(|class| {
                        matches!(
                            class.severity,
                            plasm_core::schema::DataClassSeverity::Sensitive
                                | plasm_core::schema::DataClassSeverity::Critical
                        )
                    })
            })
    }

    fn confidential_expression(&self, expression: &PyExpr, visited: &mut BTreeSet<String>) -> bool {
        match expression {
            PyExpr::Attribute(attr) => self
                .expression_owner(&attr.value)
                .is_some_and(|owner| self.confidential_field(&owner, attr.attr.as_str())),
            PyExpr::Name(name) => {
                self.confidential_binding(self.scoped_binding(name.id.as_str()), visited)
            }
            _ => false,
        }
    }

    fn confidential_binding(&self, binding: &str, visited: &mut BTreeSet<String>) -> bool {
        use super::super::types::DagNodeSource;
        if !visited.insert(binding.to_owned()) {
            return false;
        }
        let Some(node) = self.state.get(binding) else {
            return false;
        };
        match &node.source {
            DagNodeSource::ScalarExtract { source, wire } => {
                super::super::schema_validate::resolve_qualified_entity_for_dag_source(
                    &self.state,
                    &[],
                    source.clone(),
                )
                .is_some_and(|owner| self.confidential_field(&owner, wire))
            }
            DagNodeSource::Data(PlasmDataValue::NodeSymbol { node, .. })
            | DagNodeSource::Derive {
                value: PlasmDataValue::NodeSymbol { node, .. },
                ..
            } => self.confidential_binding(node, visited),
            _ => false,
        }
    }

    pub(super) fn record_sensitive_comparisons(&mut self, expression: &PyExpr) {
        use ruff_python_ast::visitor::{self, Visitor};
        struct Comparisons<'a, 's>(&'a mut Lower<'s>);
        impl<'a> Visitor<'a> for Comparisons<'_, '_> {
            fn visit_expr(&mut self, expression: &'a PyExpr) {
                if let PyExpr::Compare(compare) = expression {
                    for operands in compare.operands.windows(2) {
                        for (field, value) in
                            [(&operands[0], &operands[1]), (&operands[1], &operands[0])]
                        {
                            let sensitive =
                                self.0.confidential_expression(field, &mut BTreeSet::new());
                            if sensitive {
                                self.0.record_operand(value, OperandMeaning::Sensitive);
                            }
                        }
                    }
                }
                visitor::walk_expr(self, expression);
            }
        }
        Comparisons(self).visit_expr(expression);
    }

    /// Identity syntax supplies the selected Get's declared receiver inputs.
    /// Entity field prose and transport parameter spelling are not reuse policy.
    pub(super) fn record_get_identity_operand(
        &mut self,
        expression: &PyExpr,
        owner: &plasm_core::symbol_tuning::EntityBinding,
        cap: &CapabilitySchema,
        entity: &plasm_core::schema::EntityDef,
        slot: Option<&str>,
        cgs: &plasm_core::CGS,
    ) {
        use plasm_core::scope_entity_ref_infer::{classify_scope_param_supply, ScopeParamSupply};
        let mut recorded = false;
        for field in cap.scope_params() {
            let supplied = match classify_scope_param_supply(entity, cap, field, cgs) {
                ScopeParamSupply::ReceiverIdentity => slot.is_none_or(|slot| field.name == slot),
                ScopeParamSupply::ReceiverEntityRef => true,
                ScopeParamSupply::Explicit => false,
            };
            if supplied {
                self.record_operand(expression, input_meaning(owner, cap, field, cgs));
                recorded = true;
            }
        }
        if !recorded {
            self.record_operand(expression, OperandMeaning::Unclassified);
        }
    }

    pub(super) fn record_invocation_operand(
        &mut self,
        expression: &PyExpr,
        owner: &plasm_core::symbol_tuning::EntityBinding,
        cap: &CapabilitySchema,
        wire: &str,
    ) {
        let meaning = if inputs::is_union_tag(cap, wire) {
            OperandMeaning::Fixed(FixedOperand::Discriminator)
        } else {
            cap.invocation_object_fields()
                .chain(cap.scope_params().iter())
                .chain(cap.inputs.controls.0.iter())
                .find(|field| field.name == wire)
                .and_then(|field| {
                    self.es
                        .contexts_by_entry
                        .get(owner.entry_id.as_str())
                        .map(|context| input_meaning(owner, cap, field, &context.cgs))
                })
                .unwrap_or(OperandMeaning::Unclassified)
        };
        self.record_operand(expression, meaning);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_and_lane_evidence_determine_role_not_field_spelling() {
        let root = std::path::PathBuf::from(crate::test_manifest_dir());
        let cgs = plasm_core::loader::load_schema_dir(
            &root.join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .unwrap();
        let cap = cgs.get_capability("langtag_query").unwrap();
        let field = &cap.scope_params()[0];
        let owner = plasm_core::symbol_tuning::EntityBinding {
            entry_id: "langmatrix".into(),
            entity: "LangTag".into(),
        };
        assert!(matches!(
            input_meaning(&owner, cap, field, &cgs),
            OperandMeaning::Data {
                kind: DataOperand::EntityIdentity,
                ..
            }
        ));
        let mut renamed = field.clone();
        renamed.name = "p999".into();
        assert!(matches!(
            input_meaning(&owner, cap, &renamed, &cgs),
            OperandMeaning::Data { .. }
        ));
        renamed.sink_class = Some(plasm_core::SinkClassName::new("credential").unwrap());
        assert_eq!(
            input_meaning(&owner, cap, &renamed, &cgs),
            OperandMeaning::Sensitive
        );
        let create = cgs.get_capability("langitem_create").unwrap();
        let scalar = create.invocation_object_fields().next().unwrap();
        assert_eq!(
            input_meaning(&owner, create, scalar, &cgs),
            OperandMeaning::Unclassified
        );
        for spelling in ["currency", "provider", "token", "recipient", "p42"] {
            let mut renamed_scalar = scalar.clone();
            renamed_scalar.name = spelling.into();
            assert_eq!(
                input_meaning(&owner, create, &renamed_scalar, &cgs),
                OperandMeaning::Unclassified
            );
        }
        let mut enumerated = cgs.clone();
        let plasm_core::InputFieldWire::Registry(key) = &scalar.wire else {
            panic!("matrix named value")
        };
        enumerated
            .values
            .get_mut(key.as_str())
            .unwrap()
            .allowed_values = Some(vec!["provider-a".into(), "provider-b".into()]);
        assert_eq!(
            input_meaning(&owner, create, scalar, &enumerated),
            OperandMeaning::Fixed(FixedOperand::DomainConstraint)
        );
        let mut controlled = cap.clone();
        controlled.inputs.controls.0.push(field.clone());
        assert_eq!(
            input_meaning(&owner, &controlled, field, &cgs),
            OperandMeaning::Fixed(FixedOperand::InvocationControl)
        );
    }
}
