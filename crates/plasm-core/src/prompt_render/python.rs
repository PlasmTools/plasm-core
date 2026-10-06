//! Python interface cards built from catalog types and session exposure, never TSV.
//!
//! Delivery is explicit: prepare a wave, send it, then retain its returned state.
//! Retrying against the same previous state produces exactly the same wave.
use std::collections::{BTreeMap, BTreeSet};

use crate::python_row_shape::PythonRowShape;
use crate::symbol_tuning::{SymbolMap, TeachingExposureSession};
use crate::value_contract::ValueContract;
use crate::{CapabilityKind, FieldType, InputFieldWire, OutputType, ValueDomainKey, CGS};
use thiserror::Error;

pub const LANGUAGE: &str = include_str!("assets/python-plasm-dag.txt");

#[derive(Debug, Error)]
pub enum PythonTeachingError {
    #[error("Python teaching language changed within a pinned session")]
    LanguageChanged,
    #[error("member symbol `{symbol}` changed ownership or disappeared")]
    MemberSymbolChanged { symbol: String },
    #[error("entity symbol `{symbol}` changed ownership")]
    EntitySymbolChanged { symbol: String },
    #[error("catalog `{entry}` is absent from teaching exposure")]
    MissingCatalog { entry: String },
    #[error("catalog `{entry}` changed within a Python session")]
    CatalogChanged { entry: String },
    #[error("teaching removed previously delivered members from `{symbol}`")]
    RemovedMembers { symbol: String },
    #[error("teaching removed declaration `{symbol}`")]
    RemovedDeclaration { symbol: String },
    #[error("value symbol `{symbol}` changed meaning")]
    ValueSymbolChanged { symbol: String },
    #[error("declaration `{symbol}` changed without adding members")]
    DeclarationChangedWithoutMembers { symbol: String },
    #[error("named value `{key}` is absent from catalog")]
    MissingValue { key: String },
    #[error("named value `{key}` has an invalid typed contract: {source}")]
    InvalidValueContract {
        key: String,
        #[source]
        source: crate::value_contract::ValueContractError,
    },
    #[error("named value `{key}` cannot be serialized")]
    SerializeValueDomain {
        key: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("domain declaration `{symbol}` conflicts with an earlier declaration")]
    ConflictingDomainDeclaration { symbol: String },
    #[error("entity `{entity}` is absent from catalog")]
    MissingEntity { entity: String },
    #[error("wire name `{name}` is not a valid Python identifier")]
    InvalidPythonIdentifier { name: String },
    #[error("capability `{capability}` is absent from catalog")]
    MissingCapability { capability: String },
    #[error("generated capability signature has no opening parenthesis")]
    InvalidGeneratedSignature,
    #[error("generated capability signature has no receiver")]
    MissingGeneratedReceiver,
    #[error("generated capability signature has no body")]
    MissingGeneratedSignatureBody,
    #[error("input declaration exceeds the supported nesting depth")]
    InputNestingLimit,
    #[error("generated input schema is invalid")]
    InvalidGeneratedInputSchema {
        #[source]
        source: serde_json::Error,
    },
    #[error("Get capability `{capability}` has no entity")]
    MissingGetEntity { capability: String },
    #[error("identity field `{field}` is not typed on entity `{entity}`")]
    MissingIdentityField { entity: String, field: String },
    #[error("capability `{capability}` requires a closed-object signature witness")]
    MissingInvocationSignatureWitness { capability: String },
    #[error("output entity `{entity}` is not exposed")]
    OutputEntityNotExposed { entity: String },
    #[error("custom/status output requires a typed return witness")]
    MissingTypedReturnWitness { capability: String },
}

/// Catalog identity of a delivered member, independent of presentation layout.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
enum DeliveredMember {
    Relation(String),
    Capability(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PythonTeachingState {
    language: String,
    #[serde(with = "domain_symbol_wire")]
    domain_symbols: BTreeMap<(String, String), String>,
    declarations: BTreeMap<String, String>,
    delivered_members: BTreeMap<String, BTreeSet<DeliveredMember>>,
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
    /// New definitions and additive member declarations; delivered members are absent.
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
) -> Result<PythonTeachingWave, PythonTeachingError> {
    if !previous.language.is_empty() && previous.language != LANGUAGE {
        return Err(PythonTeachingError::LanguageChanged);
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
            return Err(PythonTeachingError::MemberSymbolChanged {
                symbol: symbol.clone(),
            });
        }
    }
    let mut renderer = Renderer {
        exposure,
        symbols: &symbols,
        definitions: BTreeMap::new(),
        delivered_members: BTreeMap::new(),
        coverage: Vec::new(),
        domain_symbols: previous.domain_symbols.clone(),
        value_contracts: BTreeMap::new(),
    };
    let mut entity_deltas = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    let mut entity_bindings = BTreeMap::new();
    for row in symbols.exposed_entity_symbol_rows() {
        let binding = (row.entry_id.clone(), row.entity.clone());
        if previous
            .entity_bindings
            .get(&row.symbol)
            .is_some_and(|old| old != &binding)
        {
            return Err(PythonTeachingError::EntitySymbolChanged {
                symbol: row.symbol.clone(),
            });
        }
        entity_bindings.insert(row.symbol.clone(), binding);
        let cgs = exposure
            .catalog_cgs_for_entry(&row.entry_id)
            .ok_or_else(|| PythonTeachingError::MissingCatalog {
                entry: row.entry_id.clone(),
            })?;
        let hash = cgs.catalog_cgs_hash_hex();
        if previous
            .catalog_hashes
            .get(&row.entry_id)
            .is_some_and(|old| old != &hash)
        {
            return Err(PythonTeachingError::CatalogChanged {
                entry: row.entry_id.clone(),
            });
        }
        hashes.insert(row.entry_id.clone(), hash);
        let complete = renderer.entity(cgs, &row.entry_id, &row.entity, &row.symbol, None)?;
        if let Some(delivered) = previous.delivered_members.get(&row.symbol) {
            let current = &renderer.delivered_members[&row.symbol];
            if !delivered.is_subset(current) {
                return Err(PythonTeachingError::RemovedMembers {
                    symbol: row.symbol.clone(),
                });
            }
            if current != delivered {
                entity_deltas.insert(
                    row.symbol.clone(),
                    renderer.entity(
                        cgs,
                        &row.entry_id,
                        &row.entity,
                        &row.symbol,
                        Some(delivered),
                    )?,
                );
            }
        }
        renderer.definitions.insert(row.symbol.clone(), complete);
    }
    for (symbol, old) in &previous.declarations {
        let new = renderer.definitions.get(symbol).ok_or_else(|| {
            PythonTeachingError::RemovedDeclaration {
                symbol: symbol.clone(),
            }
        })?;
        if symbol.starts_with('v') && old != new {
            return Err(PythonTeachingError::ValueSymbolChanged {
                symbol: symbol.clone(),
            });
        }
    }
    let mut declarations = String::new();
    // Domain aliases precede entity definitions, independent of lexical e#/v# order.
    for prefix in ['v', 's', 'e'] {
        for (symbol, body) in &renderer.definitions {
            if symbol.starts_with(prefix) && previous.declarations.get(symbol) != Some(body) {
                if previous.declarations.contains_key(symbol) {
                    if let Some(delta) = entity_deltas.get(symbol) {
                        declarations.push_str(delta);
                    } else {
                        return Err(PythonTeachingError::DeclarationChangedWithoutMembers {
                            symbol: symbol.clone(),
                        });
                    }
                } else {
                    declarations.push_str(body);
                }
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
            delivered_members: renderer.delivered_members,
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
    delivered_members: BTreeMap<String, BTreeSet<DeliveredMember>>,
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
    ) -> Result<String, PythonTeachingError> {
        // Existing TSV v# names deduplicate structural shapes across domains.
        // Python aliases carry nominal domain identity, so allocate them separately
        // within this language-pinned session. e#/m#/r# remain shared allocations.
        let next = format!("v{}", self.domain_symbols.len() + 1);
        let symbol = self
            .domain_symbols
            .entry((entry.into(), key.as_str().into()))
            .or_insert(next)
            .clone();
        let value =
            cgs.values
                .get(key.as_str())
                .ok_or_else(|| PythonTeachingError::MissingValue {
                    key: key.as_str().to_owned(),
                })?;
        let contract = ValueContract::from_domain(cgs, entry, key).map_err(|source| {
            PythonTeachingError::InvalidValueContract {
                key: key.as_str().to_owned(),
                source,
            }
        })?;
        self.value_contracts
            .insert(symbol.clone(), contract.clone());
        let mut ty = contract.python_type();
        if let Some(items) = &value.array_items {
            let element = self.domain(cgs, entry, "", "", items.kind.registry_key())?;
            ty = format!("list[{element}]");
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
        body.push_str(&format!("{symbol}: {ty}"));
        // Shape is already in the alias. Keep non-redundant profile/constraint
        // meaning in comments; the complete recursive contract remains structured.
        let mut details = serde_json::to_value(&value.domain).map_err(|source| {
            PythonTeachingError::SerializeValueDomain {
                key: key.as_str().to_owned(),
                source,
            }
        })?;
        if let Some(fields) = details.as_object_mut() {
            if !matches!(value.field_type, FieldType::EntityRef { .. }) {
                fields.remove("kernel");
            }
            fields.remove("enum"); // Literal[...] above retains every enum member.
            if fields.len() == 1
                && fields
                    .get("profile")
                    .and_then(|v| v.as_str())
                    .is_some_and(|profile| {
                        !profile.is_empty()
                            && profile
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    })
            {
                body.push_str(&format!(
                    " @{}",
                    comment_text(fields["profile"].as_str().unwrap())
                ));
            } else if !fields.is_empty() {
                body.push_str(&format!(" {}", comment_text(&details.to_string())));
            }
        }
        if !value.description.is_empty() {
            body.push_str(" # ");
            body.push_str(&comment_text(&value.description));
        }
        body.push('\n');
        if let Some(existing) = self.definitions.insert(symbol.clone(), body.clone()) {
            if existing != body {
                return Err(PythonTeachingError::ConflictingDomainDeclaration { symbol });
            }
        }
        Ok(symbol)
    }

    fn entity(
        &mut self,
        cgs: &CGS,
        entry: &str,
        name: &str,
        symbol: &str,
        delivered: Option<&BTreeSet<DeliveredMember>>,
    ) -> Result<String, PythonTeachingError> {
        let entity = cgs
            .get_entity(name)
            .ok_or_else(|| PythonTeachingError::MissingEntity {
                entity: name.to_owned(),
            })?;
        let mut body = String::new();
        if delivered.is_some() {
            body.push_str(&format!(
                "# Add members to {symbol}; prior declarations remain valid.\n"
            ));
        } else {
            comment(
                &mut body,
                "",
                &format!("{entry}::{name} — {}", entity.description),
            );
        }
        self.delivered_members.entry(symbol.into()).or_default();
        body.push_str(&format!("{symbol}:\n"));
        let mut previous_plain_field = false;
        for (name, field) in entity.fields.iter().filter(|_| delivered.is_none()) {
            identifier(name.as_str())?;
            let ty = self.domain(
                cgs,
                entry,
                entity.name.as_str(),
                name.as_str(),
                field.kind.registry_key(),
            )?;
            let has_comment = cgs
                .values
                .get(field.kind.registry_key().as_str())
                .is_none_or(|v| v.description != field.description)
                && !field.description.is_empty();
            if has_comment {
                comment(&mut body, "    ", &field.description);
            }
            let prefix = if previous_plain_field && !has_comment {
                body.pop();
                "; "
            } else {
                "    "
            };
            body.push_str(&format!(
                "{prefix}{name}: {ty}{}\n",
                if field.required { "" } else { " | None" }
            ));
            previous_plain_field = !has_comment;
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
            let member = DeliveredMember::Relation(relation.wire.clone());
            self.delivered_members
                .entry(symbol.into())
                .or_default()
                .insert(member.clone());
            if delivered.is_some_and(|members| members.contains(&member)) {
                continue;
            }
            comment(
                &mut body,
                "    ",
                &format!("{}: {}", relation.wire, schema.description),
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
        let mut operation_notes: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut signatures = Vec::new();
        for key in &self.exposure.surface.capabilities {
            if key.entry_id != entry || key.domain.as_str() != name {
                continue;
            }
            let member = DeliveredMember::Capability(key.capability.to_string());
            self.delivered_members
                .entry(symbol.into())
                .or_default()
                .insert(member.clone());
            if delivered.is_some_and(|members| members.contains(&member)) {
                continue;
            }
            let cap = cgs.get_capability(key.capability.as_str()).ok_or_else(|| {
                PythonTeachingError::MissingCapability {
                    capability: key.capability.to_string(),
                }
            })?;
            let mut meaning = String::new();
            comment(&mut meaning, "", &cap.description);
            let result = self.signature(cgs, entry, symbol, cap);
            let signature = result.as_ref().ok().cloned();
            let unavailable = match result {
                Ok(signature) => {
                    let seat = if matches!(
                        cap.kind,
                        CapabilityKind::Get | CapabilityKind::Query | CapabilityKind::Search
                    ) || !cap.requires_receiver()
                    {
                        symbol
                    } else {
                        "row"
                    };
                    let method = capability_method_name(cgs, self.symbols, entry, cap);
                    let target = format!("{seat}.{method}");
                    let notes: Vec<String> = meaning
                        .lines()
                        .chain(
                            signature
                                .lines()
                                .map(str::trim)
                                .filter(|line| line.starts_with("# ")),
                        )
                        .map(str::to_owned)
                        .collect();
                    for note in &notes {
                        operation_notes
                            .entry(note.clone())
                            .or_default()
                            .push(target.clone());
                    }
                    let mut compact = String::new();
                    for line in signature.lines().map(str::trim) {
                        if let Some(declaration) = line.strip_prefix("def ") {
                            let (method, arguments) = declaration
                                .split_once('(')
                                .ok_or(PythonTeachingError::InvalidGeneratedSignature)?;
                            let arguments = arguments
                                .strip_prefix("cls, ")
                                .or_else(|| arguments.strip_prefix("self, "))
                                .or_else(|| arguments.strip_prefix("cls"))
                                .or_else(|| arguments.strip_prefix("self"))
                                .ok_or(PythonTeachingError::MissingGeneratedReceiver)?;
                            let arguments = arguments
                                .strip_suffix(": ...")
                                .ok_or(PythonTeachingError::MissingGeneratedSignatureBody)?;
                            // Reference cards teach named inputs once in the tool contract.
                            // Preserve the executable signature in coverage metadata.
                            let arguments = arguments.strip_prefix("*, ").unwrap_or(arguments);
                            compact.push_str(&format!("  {seat}.{method}({arguments}\n"));
                        }
                    }
                    signatures.push((compact, notes));
                    None
                }
                Err(reason) => {
                    let reason = reason.to_string();
                    body.push_str(&meaning);
                    comment(
                        &mut body,
                        "    ",
                        &format!("Unavailable in this Python card profile: {reason}"),
                    );
                    Some(reason)
                }
            };
            if delivered.is_none() {
                self.coverage.push(PythonCapabilityCoverage {
                    entry_id: entry.into(),
                    capability: cap.name.to_string(),
                    unavailable,
                    signature,
                });
            }
        }
        // Group identical CGS prose with explicit owners; no meaning is discarded.
        for (note, targets) in &mut operation_notes {
            targets.sort();
            targets.dedup();
            if targets.len() > 1 {
                body.push_str(&format!("  {} {note}\n", targets.join(",")));
            }
        }
        for (signature, notes) in signatures {
            for note in notes {
                if operation_notes[&note].len() == 1 {
                    body.push_str(&format!("  {note}\n"));
                }
            }
            body.push_str(&signature);
        }
        Ok(body)
    }

    fn input_field(
        &mut self,
        cgs: &CGS,
        entry: &str,
        entity: &str,
        path: &str,
        field: &crate::InputFieldSchema,
        depth: usize,
    ) -> Result<String, PythonTeachingError> {
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
    ) -> Result<String, PythonTeachingError> {
        use crate::InputType;
        if depth > 64 {
            return Err(PythonTeachingError::InputNestingLimit);
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
                if min_length.is_none() && max_length.is_none() {
                    return Ok(format!("list[{element}]"));
                }
                format!(
                    "Annotated[list[{element}], {}]",
                    quote(&format!(
                        "length {}..{}",
                        min_length.map_or("*".into(), |n| n.to_string()),
                        max_length.map_or("*".into(), |n| n.to_string())
                    ))
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
                    let discriminator: crate::InputFieldSchema = serde_json::from_value(serde_json::json!({"name":variant.wire.field,"required":true,"input_type":{"type":"value","field_type":"string","allowed_values":[variant.wire.value]}})).map_err(|source| PythonTeachingError::InvalidGeneratedInputSchema { source })?;
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
    ) -> Result<String, PythonTeachingError> {
        self.signature_with_path(cgs, entry, owner, cap, cap.name.as_str())
    }

    fn signature_with_path(
        &mut self,
        cgs: &CGS,
        entry: &str,
        owner: &str,
        cap: &crate::CapabilitySchema,
        signature_path: &str,
    ) -> Result<String, PythonTeachingError> {
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
                    })).map_err(|source| PythonTeachingError::InvalidGeneratedInputSchema { source })?);
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
            let entity = cgs.get_entity(cap.domain.as_str()).ok_or_else(|| {
                PythonTeachingError::MissingGetEntity {
                    capability: cap.name.to_string(),
                }
            })?;
            let keys = if entity.key_vars.len() > 1 {
                entity.key_vars.clone()
            } else {
                vec![entity.id_field.clone()]
            };
            if keys.len() > 1 {
                params.push("*".into());
            }
            for key in keys {
                let field = entity.fields.get(key.as_str()).ok_or_else(|| {
                    PythonTeachingError::MissingIdentityField {
                        entity: entity.name.to_string(),
                        field: key.to_string(),
                    }
                })?;
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
                comment(
                    &mut body,
                    "    ",
                    &format!(
                        "session({}:{ty}) before Get; not a get(...) argument",
                        field.name
                    ),
                );
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
                    return Err(PythonTeachingError::MissingInvocationSignatureWitness {
                        capability: cap.name.to_string(),
                    });
                }
            }
            let source_call = matches!(cap.kind, CapabilityKind::Query | CapabilityKind::Search);
            let fields: Vec<_> = if source_call {
                cap.inputs.query_source_fields().collect()
            } else {
                cap.input_fields().collect()
            };
            for field in fields {
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
                if let Some(effect) = field.selection_effect {
                    comment(
                        &mut body,
                        "    ",
                        &format!("{} {}", field.name, effect.semantic_gloss()),
                    );
                }
                if let Some(description) = &field.description {
                    let shared = match &field.wire {
                        InputFieldWire::Registry(key) => cgs.values.get(key.as_str()),
                        InputFieldWire::Inline(_) => None,
                    };
                    if shared.is_none_or(|value| value.description != *description) {
                        comment(&mut body, "    ", &format!("{}: {description}", field.name));
                    }
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
                    .ok_or_else(|| PythonTeachingError::OutputEntityNotExposed {
                        entity: entity_type.to_string(),
                    })?;
                let many = matches!(
                    cap.output_schema.as_ref().map(|s| &s.output_type),
                    Some(OutputType::Collection { .. })
                );
                if many {
                    PythonRowShape::Rows
                } else {
                    PythonRowShape::Singleton
                }
                .card_annotation(&target)
            }
            Some(_) => {
                return Err(PythonTeachingError::MissingTypedReturnWitness {
                    capability: cap.name.to_string(),
                })
            }
            None => match cap.kind {
                CapabilityKind::Get => PythonRowShape::Singleton.card_annotation(owner),
                CapabilityKind::Query | CapabilityKind::Search => {
                    PythonRowShape::Rows.card_annotation(owner)
                }
                CapabilityKind::Create => PythonRowShape::Singleton.card_annotation(owner),
                _ if !cap.provides.is_empty() => PythonRowShape::Singleton.card_annotation(owner),
                _ => "EffectAck".into(),
            },
        };
        let write_returns_entity = matches!(
            cap.kind,
            CapabilityKind::Create
                | CapabilityKind::Update
                | CapabilityKind::Delete
                | CapabilityKind::Action
        ) && matches!(
            cap.output_schema.as_ref().map(|schema| &schema.output_type),
            Some(OutputType::Entity { .. } | OutputType::Collection { .. }) | None
        );
        let result_fields = cgs.effective_provides(cap);
        if write_returns_entity && !result_fields.is_empty() {
            comment(
                &mut body,
                "    ",
                &format!(
                    "Result fields available immediately: {}",
                    result_fields.join(", ")
                ),
            );
        }
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

// Catalog prose remains data on a comment line. Escape controls and fence
// delimiters without JSON-string quoting ordinary prose or embedded constraints.
fn comment_text(text: &str) -> String {
    text.chars()
        .flat_map(|ch| {
            if ch == '`' || ch.is_control() {
                ch.escape_unicode().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}

fn comment(out: &mut String, indent: &str, text: &str) {
    for line in text.lines().filter(|line| !line.is_empty()) {
        out.push_str(&format!("{indent}# {}\n", comment_text(line)));
    }
}

fn identifier(value: &str) -> Result<(), PythonTeachingError> {
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
        return Err(PythonTeachingError::InvalidPythonIdentifier {
            name: value.to_owned(),
        });
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
