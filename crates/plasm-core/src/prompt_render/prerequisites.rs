//! Declared prerequisite guidance uses the same session symbols as ordinary teaching.

use crate::prerequisites::{
    CapabilityRef, InputPath, PrerequisiteClosure, Provider, ResolvedArgument,
};
use crate::schema::EntityDef;
use crate::symbol_tuning::SymbolRender;
use crate::{CapabilityKind, CapabilitySchema, CGS};
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
        let filled_get = renderer.filled_get_identity_acquisition(acquisition, provider)?;
        lines.push(format!(
            "Acquisition {}: {}",
            acquisition.id,
            filled_get
                .clone()
                .map_or_else(|| renderer.capability(&acquisition.capability), Ok)?
        ));
        for (port, argument) in &acquisition.arguments {
            let input = provider
                .inputs
                .get(port)
                .ok_or("missing provider input mapping")?;
            if filled_get.is_some()
                && renderer.is_get_identity_input(&acquisition.capability, input)?
            {
                continue;
            }
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
        Ok(taught_capability_seat(
            cap,
            &entity,
            &method,
            self.catalogs.get(&reference.catalog).copied(),
            &self.identity_wire(&reference.catalog, cap),
        ))
    }

    fn get_identity_field(&self, reference: &CapabilityRef) -> Result<Option<String>, String> {
        let cap = self.schema(reference)?;
        if cap.kind != CapabilityKind::Get {
            return Ok(None);
        }
        let Some(cgs) = self.catalogs.get(&reference.catalog).copied() else {
            return Ok(None);
        };
        if crate::sole_nullary_singleton_get(cgs, cap.domain.as_str())
            .is_some_and(|sole| sole.name == cap.name)
        {
            return Ok(None);
        }
        if let Some(derived) = cap.derived.as_ref() {
            return Ok(Some(derived.identity_field.clone()));
        }
        Ok(cgs
            .get_entity(cap.domain.as_str())
            .map(|ent| ent.id_field.as_str().to_string()))
    }

    fn is_get_identity_input(
        &self,
        reference: &CapabilityRef,
        input: &InputPath,
    ) -> Result<bool, String> {
        let Some(identity) = self.get_identity_field(reference)? else {
            return Ok(false);
        };
        Ok(input.path.first().is_some_and(|head| head == &identity))
    }

    fn filled_get_identity_acquisition(
        &self,
        acquisition: &crate::prerequisites::Acquisition,
        provider: &Provider,
    ) -> Result<Option<String>, String> {
        let Some(identity) = self.get_identity_field(&acquisition.capability)? else {
            return Ok(None);
        };
        let mut found: Option<&serde_json::Value> = None;
        for (port, argument) in &acquisition.arguments {
            let input = provider
                .inputs
                .get(port)
                .ok_or("missing provider input mapping")?;
            if input.path.first().is_some_and(|head| head == &identity) {
                match argument {
                    ResolvedArgument::Constant { value } => {
                        if found.is_some() {
                            return Ok(None);
                        }
                        found = Some(value);
                    }
                    _ => return Ok(None),
                }
            }
        }
        let Some(value) = found else {
            return Ok(None);
        };
        let cap = self.schema(&acquisition.capability)?;
        let entity = self
            .symbols
            .entity_sym_for(&acquisition.capability.catalog, cap.domain.as_str());
        let wire = self.identity_wire(&acquisition.capability.catalog, cap);
        let ent = self
            .catalogs
            .get(&acquisition.capability.catalog)
            .and_then(|cgs| cgs.get_entity(cap.domain.as_str()));
        Ok(Some(format_get_identity_call(
            &entity,
            &wire,
            ent,
            self.catalogs.get(&acquisition.capability.catalog).copied(),
            value,
        )))
    }

    fn identity_wire(&self, catalog: &str, cap: &CapabilitySchema) -> String {
        let Some(cgs) = self.catalogs.get(catalog).copied() else {
            return String::new();
        };
        let Some(ent) = cgs.get_entity(cap.domain.as_str()) else {
            return String::new();
        };
        if ent.id_field.as_str().is_empty() {
            return String::new();
        }
        self.symbols
            .ident_sym_entity_field_for(catalog, cap.domain.as_str(), ent.id_field.as_str())
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

/// Same fetch/mutator seat the teaching table already showed — never `e# / m#` for reads.
fn taught_capability_seat(
    cap: &CapabilitySchema,
    entity: &str,
    method: &str,
    cgs: Option<&CGS>,
    id_wire: &str,
) -> String {
    match cap.kind {
        CapabilityKind::Get => {
            let pathless = cgs
                .and_then(|g| crate::sole_nullary_singleton_get(g, cap.domain.as_str()))
                .is_some_and(|sole| sole.name == cap.name);
            if pathless {
                entity.to_string()
            } else {
                taught_get_identity_seat(
                    entity,
                    id_wire,
                    cgs.and_then(|g| g.get_entity(cap.domain.as_str())),
                    cgs,
                )
            }
        }
        CapabilityKind::Query => format!("{entity}{{…}}"),
        CapabilityKind::Search => format!("{entity}~\"<query>\""),
        _ if cap.requires_receiver() => crate::taught_seat::taught_identity_mutator_basename(
            entity,
            method,
            id_wire,
            cgs.and_then(|g| g.get_entity(cap.domain.as_str())),
            cgs,
        ),
        _ => format!("{entity}.{method}"),
    }
}

fn taught_get_identity_seat(
    entity: &str,
    id_wire: &str,
    ent: Option<&EntityDef>,
    cgs: Option<&CGS>,
) -> String {
    crate::taught_seat::taught_get_identity_receiver(entity, id_wire, ent, cgs)
}

fn format_get_identity_call(
    entity: &str,
    id_wire: &str,
    ent: Option<&EntityDef>,
    cgs: Option<&CGS>,
    value: &serde_json::Value,
) -> String {
    let token_braces = ent.is_some_and(|e| cgs.is_some_and(|g| e.teaches_token_identity_braces(g)))
        && !id_wire.is_empty();
    let lit = match value {
        serde_json::Value::String(s) => {
            let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{escaped}\"")
        }
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        _ => {
            return taught_get_identity_seat(entity, id_wire, ent, cgs);
        }
    };
    if token_braces {
        format!("{entity}{{{id_wire}={lit}}}")
    } else {
        format!("{entity}({lit})")
    }
}
