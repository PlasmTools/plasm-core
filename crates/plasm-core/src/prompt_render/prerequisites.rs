//! Declared prerequisite guidance uses the same session symbols as ordinary teaching.

use crate::prerequisites::{CapabilityRef, InputPath, PrerequisiteClosure, ResolvedArgument};
use crate::symbol_tuning::SymbolRender;
use crate::{CapabilitySchema, CGS};
use std::collections::BTreeMap;

/// Explain input/output bindings after the host has exposed every capability in the closure.
/// This renderer is pure: acquisition remains ordinary agent-authored execution.
pub fn render_prerequisite_bindings(
    closure: &PrerequisiteClosure,
    catalogs: &BTreeMap<String, &CGS>,
    symbols: &dyn SymbolRender,
) -> Result<String, String> {
    if closure.edges.is_empty() {
        return Ok(String::new());
    }
    let renderer = BindingRenderer {
        catalogs,
        symbols,
        closure,
    };
    let mut lines = vec![
        "Declared prerequisites".into(),
        "Write acquisition calls using the ordinary capability syntax in the teaching table. Bind their returned fields into the indicated existing inputs. Acquisition calls participate in normal plan review and execution.".into(),
    ];
    for acquisition in &closure.acquisitions {
        let provider = catalogs
            .get(&acquisition.provider_catalog)
            .and_then(|c| c.prerequisites.providers.get(&acquisition.provider))
            .ok_or("missing taught provider")?;
        lines.push(format!(
            "Acquisition {}: {}",
            acquisition.id,
            renderer.capability(&acquisition.capability)?
        ));
        for (port, argument) in &acquisition.arguments {
            let input = provider
                .inputs
                .get(port)
                .ok_or("missing provider input mapping")?;
            lines.push(format!(
                "  {} <- {}",
                renderer.input(&acquisition.capability, input)?,
                renderer.argument(argument)?
            ));
        }
    }
    for edge in &closure.edges {
        let consumer_instance = edge.consumer_instance.as_deref().unwrap_or("business");
        lines.push(format!(
            "Required by {} ({consumer_instance}), requirement {}:",
            renderer.capability(&edge.consumer)?,
            edge.requirement.id
        ));
        for binding in &edge.requirement.bindings {
            let field = edge
                .provider_outputs
                .get(&binding.output)
                .ok_or("missing output mapping")?;
            lines.push(format!(
                "  {} <- acquisition {} output {}",
                renderer.input(&edge.consumer, &binding.input)?,
                edge.provider_instance,
                renderer.output(&edge.provider, field)?
            ));
        }
    }
    Ok(lines.join("\n"))
}

struct BindingRenderer<'a> {
    catalogs: &'a BTreeMap<String, &'a CGS>,
    symbols: &'a dyn SymbolRender,
    closure: &'a PrerequisiteClosure,
}

impl BindingRenderer<'_> {
    fn schema(&self, reference: &CapabilityRef) -> Result<&CapabilitySchema, String> {
        self.catalogs
            .get(&reference.catalog)
            .and_then(|cgs| cgs.capabilities.get(reference.capability.as_str()))
            .ok_or_else(|| format!("missing capability {reference:?}"))
    }

    fn capability(&self, reference: &CapabilityRef) -> Result<String, String> {
        let cap = self.schema(reference)?;
        let entity = self
            .symbols
            .entity_sym_for(&reference.catalog, cap.domain.as_str());
        let method = self.symbols.method_sym_for(
            &reference.catalog,
            cap.domain.as_str(),
            &reference.capability,
        );
        if self.symbols.resolve_method_symbol_triple(&method).is_none() {
            return Err(format!(
                "prerequisite capability was not exposed: {reference:?}"
            ));
        }
        Ok(format!("{entity} / {method}"))
    }

    fn input(&self, reference: &CapabilityRef, input: &InputPath) -> Result<String, String> {
        let cap = self.schema(reference)?;
        let (head, tail) = input.path.split_first().ok_or("empty input binding")?;
        let param = if input.lane == crate::prerequisites::InputLane::Selection
            && cap
                .derived
                .as_ref()
                .is_some_and(|d| &d.identity_field == head)
        {
            self.symbols
                .ident_sym_entity_field_for(&reference.catalog, cap.domain.as_str(), head)
        } else {
            self.symbols.ident_sym_cap_param_for(
                &reference.catalog,
                cap.domain.as_str(),
                &reference.capability,
                head,
            )
        };
        let mut path = vec![param];
        path.extend(tail.iter().cloned());
        Ok(format!(
            "{} {:?}.{}",
            self.capability(reference)?,
            input.lane,
            path.join(".")
        ))
    }

    fn output(&self, reference: &CapabilityRef, field: &str) -> Result<String, String> {
        let cap = self.schema(reference)?;
        let entity = match cap.output_schema.as_ref().map(|o| &o.output_type) {
            Some(crate::schema::OutputType::Entity { entity_type }) => entity_type,
            _ => return Err("provider has no single entity output".into()),
        };
        Ok(self
            .symbols
            .ident_sym_entity_field_for(&reference.catalog, entity, field))
    }

    fn argument(&self, argument: &ResolvedArgument) -> Result<String, String> {
        match argument {
            ResolvedArgument::BusinessIdentity { consumer, field } => {
                let cap = self.schema(consumer)?;
                let symbol = self.symbols.ident_sym_entity_field_for(
                    &consumer.catalog,
                    cap.domain.as_str(),
                    field,
                );
                Ok(format!(
                    "{} target identity {symbol}",
                    self.capability(consumer)?
                ))
            }
            ResolvedArgument::Constant { value } => {
                serde_json::to_string(value).map_err(|e| e.to_string())
            }
            ResolvedArgument::BusinessInput { consumer, input } => self.input(consumer, input),
            ResolvedArgument::ProviderOutput { instance, field } => {
                let acquisition = self
                    .closure
                    .acquisitions
                    .iter()
                    .find(|a| &a.id == instance)
                    .ok_or("missing acquisition instance")?;
                Ok(format!(
                    "acquisition {instance} output {}",
                    self.output(&acquisition.capability, field)?
                ))
            }
        }
    }
}
