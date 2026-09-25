//! Python interface cards built from catalog types and session exposure, never TSV.
//!
//! Delivery is explicit: prepare a wave, send it, then retain its returned state.
//! Retrying against the same previous state produces exactly the same wave.
use std::collections::BTreeMap;

use crate::symbol_tuning::{SymbolMap, TeachingExposureSession};
use crate::value_contract::ValueContract;
use crate::{CapabilityKind, FieldType, InputFieldWire, OutputType, ValueDomainKey, CGS};

pub const LANGUAGE: &str = include_str!("assets/python-plasm-dag.txt");

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PythonTeachingState {
    language: String,
    #[serde(with = "domain_symbol_wire")]
    domain_symbols: BTreeMap<(String, String), String>,
    declarations: BTreeMap<String, String>,
    catalog_hashes: BTreeMap<String, String>,
    revision: u64,
    entity_bindings: BTreeMap<String, (String, String)>,
    member_bindings: BTreeMap<String, (String, String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PythonTeachingWave {
    pub revision: u64,
    /// Present only on first delivery. A changed language requires a new session.
    pub language: Option<&'static str>,
    /// Complete definitions of new or changed entities; unchanged definitions absent.
    pub declarations: String,
    /// Every selected capability is either declared or has an explicit reason.
    pub capabilities: Vec<PythonCapabilityCoverage>,
    pub next_state: PythonTeachingState,
    pub value_contracts: BTreeMap<String, ValueContract>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PythonCapabilityCoverage {
    pub entry_id: String,
    pub capability: String,
    pub unavailable: Option<String>,
    pub signature: Option<String>,
}

/// Canonical Python method spelling shared by teaching and generated clients.
pub fn capability_method_name(
    cgs: &CGS,
    symbols: &SymbolMap,
    entry: &str,
    cap: &crate::CapabilitySchema,
) -> String {
    match cap.kind {
        CapabilityKind::Get
            if cgs
                .primary_get_capability(cap.domain.as_str())
                .is_some_and(|primary| primary.name == cap.name) =>
        {
            "get".into()
        }
        CapabilityKind::Query
            if cgs
                .find_capabilities(cap.domain.as_str(), CapabilityKind::Query)
                .len()
                == 1 =>
        {
            "query".into()
        }
        CapabilityKind::Search
            if cgs
                .primary_search_capability(cap.domain.as_str())
                .is_some_and(|primary| primary.name == cap.name) =>
        {
            "search".into()
        }
        _ => symbols.method_sym_for(entry, cap.domain.as_str(), cap.name.as_str()),
    }
}

/// Prepare an incremental card from the session's authoritative symbol allocation.
/// This renderer does not select the host's source frontend or grant execution.
pub fn prepare_python_teaching_wave(
    exposure: &TeachingExposureSession,
    previous: &PythonTeachingState,
) -> Result<PythonTeachingWave, String> {
    if !previous.language.is_empty() && previous.language != LANGUAGE {
        return Err("Python language changed; open a new language-pinned session".into());
    }
    let symbols = exposure.to_symbol_map();
    let mut member_bindings = BTreeMap::new();
    for relation in symbols.exposed_relation_symbol_rows() {
        member_bindings.insert(
            relation.symbol,
            (relation.entry_id, relation.entity, relation.wire),
        );
    }
    for key in &exposure.surface.capabilities {
        let symbol =
            symbols.method_sym_for(&key.entry_id, key.domain.as_str(), key.capability.as_str());
        // Get/query capabilities can have canonical names instead of m# aliases.
        let identity = if symbol.starts_with('m') {
            symbol
        } else {
            format!("{}::{}::{}", key.entry_id, key.domain, key.capability)
        };
        member_bindings.insert(
            identity,
            (
                key.entry_id.clone(),
                key.domain.to_string(),
                key.capability.to_string(),
            ),
        );
    }
    for (symbol, binding) in &previous.member_bindings {
        if member_bindings.get(symbol) != Some(binding) {
            return Err(format!("member symbol {symbol} changed or was removed"));
        }
    }
    let mut renderer = Renderer {
        exposure,
        symbols: &symbols,
        definitions: BTreeMap::new(),
        coverage: Vec::new(),
        domain_symbols: previous.domain_symbols.clone(),
        value_contracts: BTreeMap::new(),
    };
    let mut hashes = BTreeMap::new();
    let mut entity_bindings = BTreeMap::new();
    for row in symbols.exposed_entity_symbol_rows() {
        let binding = (row.entry_id.clone(), row.entity.clone());
        if previous
            .entity_bindings
            .get(&row.symbol)
            .is_some_and(|old| old != &binding)
        {
            return Err(format!("entity symbol {} changed ownership", row.symbol));
        }
        entity_bindings.insert(row.symbol.clone(), binding);
        let cgs = exposure
            .catalog_cgs_for_entry(&row.entry_id)
            .ok_or("missing catalog")?;
        let hash = cgs.catalog_cgs_hash_hex();
        if previous
            .catalog_hashes
            .get(&row.entry_id)
            .is_some_and(|old| old != &hash)
        {
            return Err(format!(
                "catalog {} changed within a Python session",
                row.entry_id
            ));
        }
        hashes.insert(row.entry_id.clone(), hash);
        renderer.entity(cgs, &row.entry_id, &row.entity, &row.symbol)?;
    }
    for (symbol, old) in &previous.declarations {
        let new = renderer
            .definitions
            .get(symbol)
            .ok_or_else(|| format!("teaching removed {symbol}"))?;
        if symbol.starts_with('v') && old != new {
            return Err(format!("value symbol {symbol} changed meaning"));
        }
    }
    let mut declarations = String::new();
    // Domain aliases precede entity definitions, independent of lexical e#/v# order.
    for prefix in ['v', 's', 'e'] {
        for (symbol, body) in &renderer.definitions {
            if symbol.starts_with(prefix) && previous.declarations.get(symbol) != Some(body) {
                if previous.declarations.contains_key(symbol) {
                    declarations.push_str(&format!("# Replace the complete {symbol} declaration; existing symbols retain meaning.\n"));
                }
                declarations.push_str(body);
                declarations.push('\n');
            }
        }
    }
    let language = previous.language.is_empty().then_some(LANGUAGE);
    let revision = previous.revision + u64::from(language.is_some() || !declarations.is_empty());
    Ok(PythonTeachingWave {
        revision,
        language,
        declarations,
        capabilities: renderer.coverage,
        value_contracts: renderer.value_contracts,
        next_state: PythonTeachingState {
            language: LANGUAGE.into(),
            domain_symbols: renderer.domain_symbols,
            declarations: renderer.definitions,
            catalog_hashes: hashes,
            revision,
            entity_bindings,
            member_bindings,
        },
    })
}

struct Renderer<'a> {
    exposure: &'a TeachingExposureSession,
    symbols: &'a SymbolMap,
    definitions: BTreeMap<String, String>,
    coverage: Vec<PythonCapabilityCoverage>,
    value_contracts: BTreeMap<String, ValueContract>,
    domain_symbols: BTreeMap<(String, String), String>,
}

impl Renderer<'_> {
    fn domain(
        &mut self,
        cgs: &CGS,
        entry: &str,
        _entity: &str,
        _wire: &str,
        key: &ValueDomainKey,
    ) -> Result<String, String> {
        // Existing TSV v# names deduplicate structural shapes across domains.
        // Python aliases carry nominal domain identity, so allocate them separately
        // within this language-pinned session. e#/m#/r# remain shared allocations.
        let next = format!("v{}", self.domain_symbols.len() + 1);
        let symbol = self
            .domain_symbols
            .entry((entry.into(), key.as_str().into()))
            .or_insert(next)
            .clone();
        let value = cgs.values.get(key.as_str()).ok_or("missing named value")?;
        let contract = ValueContract::from_domain(cgs, entry, key)?;
        self.value_contracts
            .insert(symbol.clone(), contract.clone());
        let mut ty = contract.python_type();
        if let Some(items) = &value.array_items {
            let element = self.domain(cgs, entry, "", "", items.kind.registry_key())?;
            ty = format!("list[{element}]");
        }
        if value.field_type == FieldType::Date {
            ty = "str".into();
        }
        if let Some(choices) = &value.allowed_values {
            let literal = format!(
                "Literal[{}]",
                choices
                    .iter()
                    .map(|v| quote(v))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            ty = if value.field_type == FieldType::MultiSelect {
                format!("list[{literal}]")
            } else {
                literal
            };
        }
        let mut body = String::new();
        comment(
            &mut body,
            "",
            &format!("{entry} value {}: {}", key.as_str(), value.description),
        );
        // Shape is already in the alias. Keep non-redundant profile/constraint
        // meaning in comments; the complete recursive contract remains structured.
        let mut details = serde_json::to_value(&value.domain).map_err(|e| e.to_string())?;
        if let Some(fields) = details.as_object_mut() {
            if !matches!(value.field_type, FieldType::EntityRef { .. }) {
                fields.remove("kernel");
            }
            fields.remove("enum"); // Literal[...] above retains every enum member.
            if !fields.is_empty() {
                comment(&mut body, "", &format!("constraints: {details}"));
            }
        }
        body.push_str(&format!("type {symbol} = {ty}\n"));
        if let Some(existing) = self.definitions.insert(symbol.clone(), body.clone()) {
            if existing != body {
                return Err(format!("conflicting domain declarations for {symbol}"));
            }
        }
        Ok(symbol)
    }

    fn entity(&mut self, cgs: &CGS, entry: &str, name: &str, symbol: &str) -> Result<(), String> {
        let entity = cgs.get_entity(name).ok_or("missing entity")?;
        let mut body = String::new();
        comment(
            &mut body,
            "",
            &format!("{entry}::{name} — {}", entity.description),
        );
        body.push_str(&format!("class {symbol}:\n"));
        for (name, field) in &entity.fields {
            identifier(name.as_str())?;
            let ty = self.domain(
                cgs,
                entry,
                entity.name.as_str(),
                name.as_str(),
                field.kind.registry_key(),
            )?;
            if cgs
                .values
                .get(field.kind.registry_key().as_str())
                .is_none_or(|v| v.description != field.description)
            {
                comment(&mut body, "    ", &field.description);
            }
            body.push_str(&format!(
                "    {name}: {ty}{}\n",
                if field.required { "" } else { " | None" }
            ));
        }
        for relation in self.symbols.exposed_relation_symbol_rows() {
            if relation.entry_id != entry || relation.entity != name {
                continue;
            }
            let Some(schema) = entity.relations.get(relation.wire.as_str()) else {
                continue;
            };
            let Some(target) = self
                .exposure
                .qualified_entity_symbol(entry, schema.target_resource.as_str())
            else {
                continue;
            };
            comment(
                &mut body,
                "    ",
                &format!("{}: {}", relation.wire, schema.description),
            );
            comment(
                &mut body,
                "    ",
                &format!(
                    "materialize: {}",
                    serde_json::to_string(&schema.materialize).map_err(|e| e.to_string())?
                ),
            );
            if matches!(schema.cardinality, crate::Cardinality::Many)
                && matches!(
                    schema.materialize,
                    None | Some(crate::schema::RelationMaterialization::Unavailable)
                )
            {
                comment(
                    &mut body,
                    "    ",
                    "Unavailable: many relation has no materialization contract.",
                );
                continue;
            }
            let cardinality = match schema.cardinality {
                crate::Cardinality::One => "Singleton",
                crate::Cardinality::Many => "Many",
            };
            body.push_str(&format!(
                "    {}: {cardinality}[{target}]\n",
                relation.symbol
            ));
        }
        for key in &self.exposure.surface.capabilities {
            if key.entry_id != entry || key.domain.as_str() != name {
                continue;
            }
            let cap = cgs
                .get_capability(key.capability.as_str())
                .ok_or("missing capability")?;
            comment(
                &mut body,
                "    ",
                &format!("{}: {}", cap.name, cap.description),
            );
            if !cap.provides.is_empty() {
                comment(
                    &mut body,
                    "    ",
                    &format!("provides: {}", cap.provides.join(", ")),
                );
            }
            let result = self.signature(cgs, entry, symbol, cap);
            let signature = result.as_ref().ok().cloned();
            let unavailable = match result {
                Ok(signature) => {
                    body.push_str(&signature);
                    None
                }
                Err(reason) => {
                    comment(
                        &mut body,
                        "    ",
                        &format!("Unavailable in this Python card profile: {reason}"),
                    );
                    Some(reason)
                }
            };
            self.coverage.push(PythonCapabilityCoverage {
                entry_id: entry.into(),
                capability: cap.name.to_string(),
                unavailable,
                signature,
            });
        }
        body.push_str("    ...\n");
        self.definitions.insert(symbol.into(), body);
        Ok(())
    }

    fn input_field(
        &mut self,
        cgs: &CGS,
        entry: &str,
        entity: &str,
        path: &str,
        field: &crate::InputFieldSchema,
        depth: usize,
    ) -> Result<String, String> {
        match &field.wire {
            InputFieldWire::Registry(key) => self.domain(cgs, entry, entity, &field.name, key),
            InputFieldWire::Inline(ty) => self.input_type(cgs, entry, entity, path, ty, depth + 1),
        }
    }

    fn input_type(
        &mut self,
        cgs: &CGS,
        entry: &str,
        entity: &str,
        path: &str,
        ty: &crate::InputType,
        depth: usize,
    ) -> Result<String, String> {
        use crate::InputType;
        if depth > 64 {
            return Err("input declaration exceeds nesting budget".into());
        }
        Ok(match ty {
            InputType::None => "None".into(),
            InputType::Value {
                field_type,
                allowed_values,
            } => {
                if let Some(values) = allowed_values {
                    let literal = format!(
                        "Literal[{}]",
                        values
                            .iter()
                            .map(|v| quote(v))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    if *field_type == FieldType::MultiSelect {
                        format!("list[{literal}]")
                    } else {
                        literal
                    }
                } else {
                    ValueContract::scalar(field_type.clone()).python_type()
                }
            }
            InputType::Array {
                element_type,
                min_length,
                max_length,
            } => {
                let element = self.input_type(
                    cgs,
                    entry,
                    entity,
                    &format!("{path}[]"),
                    element_type,
                    depth + 1,
                )?;
                format!(
                    "Annotated[list[{element}], {}]",
                    quote(&format!("length {min_length:?}..{max_length:?}"))
                )
            }
            InputType::Object {
                fields,
                additional_fields,
            } => {
                let key = (entry.to_owned(), format!("input:{entity}:{path}"));
                let next = format!("s{}", self.domain_symbols.len() + 1);
                let symbol = self.domain_symbols.entry(key).or_insert(next).clone();
                let mut members = Vec::new();
                let mut body = String::new();
                for field in fields {
                    let field_type = self.input_field(
                        cgs,
                        entry,
                        entity,
                        &format!("{path}.{}", field.name),
                        field,
                        depth + 1,
                    )?;
                    let field_type = if field.required {
                        field_type
                    } else {
                        format!("NotRequired[{field_type}]")
                    };
                    members.push(format!("{}: {field_type}", quote(&field.name)));
                    if let Some(description) = &field.description {
                        comment(&mut body, "", &format!("{}: {description}", field.name));
                    }
                    if let Some(default) = &field.default {
                        comment(
                            &mut body,
                            "",
                            &format!("{} default: {default:?}", field.name),
                        );
                    }
                }
                if *additional_fields {
                    comment(
                        &mut body,
                        "",
                        "Additional keys accept JsonValue; declared keys retain their types.",
                    );
                }
                body.push_str(&format!(
                    "{symbol} = TypedDict({}, {{{}}})\n",
                    quote(&symbol),
                    members.join(", ")
                ));
                self.definitions.insert(symbol.clone(), body);
                symbol
            }
            InputType::Union { variants } => {
                let mut types = Vec::new();
                for variant in variants {
                    let mut fields = variant.fields.clone();
                    let discriminator: crate::InputFieldSchema = serde_json::from_value(serde_json::json!({"name":variant.wire.field,"required":true,"input_type":{"type":"value","field_type":"string","allowed_values":[variant.wire.value]}})).map_err(|e| e.to_string())?;
                    fields.push(discriminator);
                    types.push(self.input_type(
                        cgs,
                        entry,
                        entity,
                        &format!("{path}.{}", variant.name),
                        &InputType::Object {
                            fields,
                            additional_fields: false,
                        },
                        depth + 1,
                    )?);
                }
                types.join(" | ")
            }
        })
    }

    fn signature(
        &mut self,
        cgs: &CGS,
        entry: &str,
        owner: &str,
        cap: &crate::CapabilitySchema,
    ) -> Result<String, String> {
        self.signature_with_path(cgs, entry, owner, cap, cap.name.as_str())
    }

    fn signature_with_path(
        &mut self,
        cgs: &CGS,
        entry: &str,
        owner: &str,
        cap: &crate::CapabilitySchema,
        signature_path: &str,
    ) -> Result<String, String> {
        // Root variants are overloads, never a flattened set of optional fields.
        for payload in [true, false] {
            let schema = if payload {
                cap.inputs.payload.as_ref()
            } else {
                cap.inputs.arguments.as_ref()
            };
            if let Some(crate::InputSchema {
                input_type: crate::InputType::Union { variants },
                ..
            }) = schema
            {
                let mut signatures = Vec::new();
                for variant in variants {
                    let mut branch = cap.clone();
                    let lane = if payload {
                        &mut branch.inputs.payload
                    } else {
                        &mut branch.inputs.arguments
                    };
                    let mut fields = variant.fields.clone();
                    fields.insert(0, serde_json::from_value(serde_json::json!({
                        "name": variant.wire.field, "required": true,
                        "input_type": {"type": "value", "field_type": "string", "allowed_values": [variant.wire.value]}
                    })).map_err(|error| error.to_string())?);
                    lane.as_mut().unwrap().input_type = crate::InputType::Object {
                        fields,
                        additional_fields: false,
                    };
                    let signature = self.signature_with_path(
                        cgs,
                        entry,
                        owner,
                        &branch,
                        &format!("{signature_path}:{}", variant.name),
                    )?;
                    // Decorators must stay adjacent; CGS comments may precede them.
                    signatures.push(format!("    @overload\n{signature}"));
                }
                return Ok(signatures.join("\n"));
            }
        }
        let mut body = String::new();
        let mut params = Vec::new();
        let root = matches!(
            cap.kind,
            CapabilityKind::Get | CapabilityKind::Query | CapabilityKind::Search
        ) || !cap.requires_receiver();
        let method = capability_method_name(cgs, self.symbols, entry, cap);
        if cap.kind == CapabilityKind::Get && cap.get_requires_identity_anchor(cgs) {
            let entity = cgs
                .get_entity(cap.domain.as_str())
                .ok_or("missing Get entity")?;
            let keys = if entity.key_vars.len() > 1 {
                entity.key_vars.clone()
            } else {
                vec![entity.id_field.clone()]
            };
            if keys.len() > 1 {
                params.push("*".into());
            }
            for key in keys {
                let field = entity
                    .fields
                    .get(key.as_str())
                    .ok_or("identity has no typed field")?;
                let ty = self.domain(
                    cgs,
                    entry,
                    cap.domain.as_str(),
                    key.as_str(),
                    field.kind.registry_key(),
                )?;
                identifier(key.as_str())?;
                params.push(format!(
                    "{}: {ty}",
                    if entity.key_vars.len() > 1 {
                        key.as_str()
                    } else {
                        "identity"
                    }
                ));
            }
            for field in cap.input_fields().filter(|field| {
                !cap.scope_params()
                    .iter()
                    .any(|scope| scope.name == field.name)
            }) {
                let ty = self.input_field(
                    cgs,
                    entry,
                    cap.domain.as_str(),
                    &format!("{}:{}", signature_path, field.name),
                    field,
                    0,
                )?;
                comment(&mut body, "    ", &format!("Requires session provision {}: {ty}; acquire it through the declared prerequisite before Get. It is not an identity argument.", field.name));
            }
        } else {
            for schema in cap.invocation_input_schemas() {
                if !matches!(
                    schema.input_type,
                    crate::InputType::Object {
                        additional_fields: false,
                        ..
                    } | crate::InputType::None
                ) {
                    return Err("non-closed-object invocation requires a signature witness".into());
                }
            }
            for field in cap.input_fields() {
                identifier(&field.name)?;
                let ty = self.input_field(
                    cgs,
                    entry,
                    cap.domain.as_str(),
                    &format!("{}:{}", signature_path, field.name),
                    field,
                    0,
                )?;
                if params.is_empty() {
                    params.push("*".into());
                }
                params.push(format!(
                    "{}: {ty}{}",
                    field.name,
                    if field.required && field.default.is_none() {
                        ""
                    } else {
                        " = ..."
                    }
                ));
                if let Some(description) = &field.description {
                    comment(&mut body, "    ", &format!("{}: {description}", field.name));
                }
                if let Some(default) = &field.default {
                    comment(
                        &mut body,
                        "    ",
                        &format!("{} default: {default:?}", field.name),
                    );
                }
            }
        }
        let result = match cap.output_schema.as_ref().map(|s| &s.output_type) {
            Some(OutputType::SideEffect { description }) => {
                comment(&mut body, "    ", description);
                "EffectAck".into()
            }
            Some(OutputType::Entity { entity_type })
            | Some(OutputType::Collection { entity_type, .. }) => {
                let target = self
                    .exposure
                    .qualified_entity_symbol(entry, entity_type)
                    .ok_or("output entity is not exposed")?;
                let many = matches!(
                    cap.output_schema.as_ref().map(|s| &s.output_type),
                    Some(OutputType::Collection { .. })
                );
                format!("{}[{target}]", if many { "Rows" } else { "Singleton" })
            }
            Some(_) => return Err("custom/status output requires a typed return witness".into()),
            None => match cap.kind {
                CapabilityKind::Get => format!("Singleton[{owner}]"),
                CapabilityKind::Query | CapabilityKind::Search => format!("Rows[{owner}]"),
                CapabilityKind::Create => format!("Singleton[{owner}]"),
                _ if !cap.provides.is_empty() => format!("Singleton[{owner}]"),
                _ => "EffectAck".into(),
            },
        };
        if cap.kind == CapabilityKind::Query
            && self
                .exposure
                .surface
                .capabilities
                .iter()
                .filter(|k| k.entry_id == entry && k.domain == cap.domain)
                .filter_map(|k| cgs.get_capability(k.capability.as_str()))
                .filter(|c| c.kind == CapabilityKind::Query)
                .count()
                > 1
        {
            body.push_str("    @overload\n");
        }
        for (lane, fields) in [
            ("scope", cap.scope_params()),
            ("selection", cap.selection_params()),
            ("controls", cap.control_params()),
        ] {
            if !fields.is_empty() {
                comment(
                    &mut body,
                    "    ",
                    &format!(
                        "{lane}: {}",
                        fields
                            .iter()
                            .map(|f| f.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
        }
        if root {
            body.push_str("    @classmethod\n");
        }
        let receiver = if root { "cls" } else { "self" };
        let tail = if params.is_empty() {
            String::new()
        } else {
            format!(", {}", params.join(", "))
        };
        body.push_str(&format!(
            "    def {method}({receiver}{tail}) -> {result}: ...\n"
        ));
        Ok(body)
    }
}

fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization")
}

fn comment(out: &mut String, indent: &str, text: &str) {
    for line in text.lines() {
        // JSON escaping prevents control characters/backticks in catalog prose
        // from changing the enclosing prompt fence or declaration structure.
        let escaped = quote(line).replace('`', "\\u0060");
        out.push_str(&format!("{indent}# CGS {}\n", escaped));
    }
}

fn identifier(value: &str) -> Result<(), String> {
    let mut chars = value.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        || [
            "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
            "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
            "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise",
            "return", "try", "while", "with", "yield",
        ]
        .contains(&value)
    {
        return Err(format!("wire name {value:?} is not a Python identifier"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

mod domain_symbol_wire {
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;
    pub fn serialize<S: serde::Serializer>(
        value: &BTreeMap<(String, String), String>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.iter().collect::<Vec<_>>().serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<(String, String), String>, D::Error> {
        let pairs = Vec::<((String, String), String)>::deserialize(deserializer)?;
        let mut result = BTreeMap::new();
        for (key, value) in pairs {
            if result.insert(key, value).is_some() {
                return Err(serde::de::Error::custom(
                    "duplicate Python domain allocation",
                ));
            }
        }
        Ok(result)
    }
}
