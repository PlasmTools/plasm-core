//! Exhaustive binding and dependency traversal for data operands.

use super::*;

impl BindOperands for crate::PlasmDataValue {
    fn bind_operands<R: OperandResolver>(&self, resolver: &mut R) -> Result<Self, R::Error> {
        use crate::PlasmDataValue as D;
        Ok(match self {
            D::Literal { .. } => self.clone(),
            D::NodeSymbol { node, alias, path } => D::Literal {
                value: resolver.node(node, alias, path)?,
            },
            D::BindingSymbol { binding, path } => D::Literal {
                value: resolver.resolve(&PlasmInputRef::row_binding(binding, path.clone()))?,
            },
            D::Symbol { path } => D::Literal {
                value: resolver.resolve(&PlasmInputRef::row_binding(
                    "_",
                    path.split('.').map(str::to_owned).collect(),
                ))?,
            },
            D::Template {
                template,
                input_bindings,
            } => D::Literal {
                value: ResolvedValue(Value::String(resolver.template(template, input_bindings)?)),
            },
            D::EntityRefKey { api, entity, key } => D::EntityRefKey {
                api: api.clone(),
                entity: entity.clone(),
                key: Box::new(key.bind_operands(resolver)?),
            },
            D::Array { items } => D::Array {
                items: items
                    .iter()
                    .map(|v| v.bind_operands(resolver))
                    .collect::<Result<_, _>>()?,
            },
            D::Object { fields } => D::Object {
                fields: fields
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), v.bind_operands(resolver)?)))
                    .collect::<Result<_, R::Error>>()?,
            },
        })
    }
}
impl BindOperands for crate::PlanPredicate {
    fn bind_operands<R: OperandResolver>(&self, resolver: &mut R) -> Result<Self, R::Error> {
        Ok(Self {
            field_path: self.field_path.clone(),
            op: self.op,
            value: self.value.bind_operands(resolver)?,
        })
    }
}

impl crate::PlasmDataValue {
    /// Structural references shared by scheduling and materialization. Literal data is opaque.
    pub fn dependencies(&self) -> std::collections::BTreeSet<String> {
        struct Dependencies(std::collections::BTreeSet<String>);
        impl OperandResolver for Dependencies {
            type Error = std::convert::Infallible;
            fn resolve(&mut self, reference: &PlasmInputRef) -> Result<ResolvedValue, Self::Error> {
                self.0.insert(
                    match reference {
                        PlasmInputRef::NodeInput { node, .. } => node,
                        PlasmInputRef::RowBinding { binding, .. } => binding,
                    }
                    .clone(),
                );
                Ok(ResolvedValue::null())
            }
            fn identity(
                &mut self,
                _: IdentityTarget<'_>,
                reference: &PlasmInputRef,
            ) -> Result<EntityId, Self::Error> {
                self.resolve(reference)?;
                Ok(EntityId::from("dependency"))
            }
            fn string(
                &mut self,
                value: &crate::program_string_template::CompiledProgramString,
            ) -> Result<String, Self::Error> {
                self.0.extend(value.roots().iter().cloned());
                Ok(String::new())
            }
            fn template(
                &mut self,
                value: &crate::program_string_template::CompiledProgramString,
                bindings: &[crate::PlanInputBinding],
            ) -> Result<String, Self::Error> {
                for root in value.roots() {
                    self.0.insert(
                        bindings
                            .iter()
                            .find(|b| &b.to == root)
                            .map(|b| &b.from)
                            .unwrap_or(root)
                            .clone(),
                    );
                }
                Ok(String::new())
            }
        }
        let mut resolver = Dependencies(Default::default());
        let Ok(_) = self.bind_operands(&mut resolver);
        resolver.0
    }
}
