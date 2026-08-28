//! Row composition primitives: canonical row identity, suffix stream classification, and compile-time row state.
//!
//! Shared by `plasm-agent-core` (plan lowering / materialization) and `plasm-runtime` (decode / chain GET).

use crate::cgs_federation::QualifiedEntityKey;
use crate::expr::{EntityKey, Ref};
use crate::expr_parser::postfix::PlasmPostfixOp;
use indexmap::IndexMap;

/// How the primary reference is encoded on the wire for this row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IdEncoding {
    #[default]
    Simple,
    Compound,
    Url,
}

/// Provenance of a row through the suffix pipeline — terminal variants forbid further relation suffixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowProvenance {
    Decoded,
    Projected { preserved_identity: bool },
    Limited { from_plural: bool },
    Aggregated,
    Render,
    Data,
}

impl RowProvenance {
    pub fn allows_relation_suffix(&self) -> bool {
        !matches!(self, Self::Aggregated | Self::Render | Self::Data)
    }
}

/// Canonical handle for “which row is this?” across compile, plan materialization, runtime decode, and relation chain GET.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowIdentity {
    pub qualified_entity: QualifiedEntityKey,
    pub reference: Ref,
    /// Scope slots populated for nested GET / path vars (owner, repo, …).
    pub ambient: IndexMap<String, String>,
    pub id_encoding: IdEncoding,
}

impl RowIdentity {
    pub fn new(
        qualified_entity: QualifiedEntityKey,
        reference: Ref,
        ambient: IndexMap<String, String>,
        id_encoding: IdEncoding,
    ) -> Self {
        Self {
            qualified_entity,
            reference,
            ambient,
            id_encoding,
        }
    }
}

/// Ordered suffix segment after a path head (Get/Query/label).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowSuffix {
    Relation { wire: String },
    Limit { count: u32 },
    Project { fields: Vec<String> },
    Sort { args: String },
    Filter { body: String },
    Aggregate { args: String },
    GroupBy { args: String },
    Dedupe { keys: String },
    Distinct { keys: Option<String> },
    With { body: String },
    Singleton,
    PageSize { n: u32 },
}

impl RowSuffix {
    pub fn from_postfix_op(op: &PlasmPostfixOp) -> Result<Self, String> {
        match op {
            PlasmPostfixOp::Limit(n) => Ok(Self::Limit { count: *n as u32 }),
            PlasmPostfixOp::Projection { fields } => Ok(Self::Project {
                fields: fields
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            }),
            PlasmPostfixOp::Sort { args } => Ok(Self::Sort { args: args.clone() }),
            PlasmPostfixOp::Filter { body } => Ok(Self::Filter { body: body.clone() }),
            PlasmPostfixOp::Aggregate { args } => Ok(Self::Aggregate { args: args.clone() }),
            PlasmPostfixOp::GroupBy { args } => Ok(Self::GroupBy { args: args.clone() }),
            PlasmPostfixOp::Dedupe { keys } => Ok(Self::Dedupe { keys: keys.clone() }),
            PlasmPostfixOp::Distinct { keys } => Ok(Self::Distinct { keys: keys.clone() }),
            PlasmPostfixOp::With { body } => Ok(Self::With { body: body.clone() }),
            PlasmPostfixOp::Singleton => Ok(Self::Singleton),
            PlasmPostfixOp::PageSize(n) => Ok(Self::PageSize { n: *n as u32 }),
        }
    }

    pub fn is_terminal_transform(&self) -> bool {
        matches!(
            self,
            Self::Sort { .. }
                | Self::Aggregate { .. }
                | Self::GroupBy { .. }
                | Self::Dedupe { .. }
                | Self::Distinct { .. }
        )
    }
}

/// Compile-time row state threaded through [`RowSuffix`] folding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowState {
    pub identity: Option<RowIdentity>,
    pub entity: String,
    pub qualified_entity: QualifiedEntityKey,
    pub provenance: RowProvenance,
}

/// Hint bundle for federated catalog resolution (teaching exposure is prompt-only).
#[derive(Debug, Clone, Copy, Default)]
pub struct ResolutionHint<'a> {
    pub owning_cgs: Option<&'a crate::schema::CGS>,
    pub source_entity: Option<&'a str>,
    pub plan_qe: Option<&'a QualifiedEntityKey>,
}

/// Opaque proof that agent-core preflight gates passed — runtime skips duplicate TC when present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreflightToken(());

impl PreflightToken {
    pub const VERIFIED: Self = Self(());
}

/// Build row identity from a decoded/materialized row reference, declared relation targets, and CGS key shape.
pub fn row_identity_from_parts(
    qualified_entity: QualifiedEntityKey,
    reference: Ref,
    relations: &IndexMap<String, Vec<Ref>>,
    id_field: &str,
    compound_key_vars: &[String],
) -> RowIdentity {
    let mut ambient = IndexMap::new();
    if let EntityKey::Compound(parts) = &reference.key {
        for (k, v) in parts {
            if !v.is_empty() {
                ambient.insert(k.clone(), v.clone());
            }
        }
    }
    let primary = reference.primary_slot_str();
    if !primary.is_empty() && compound_key_vars.is_empty() {
        ambient
            .entry(id_field.to_string())
            .or_insert_with(|| primary.clone());
    }
    for (wire, refs) in relations {
        if let Some(r) = refs.first() {
            let slot = r.primary_slot_str();
            if !slot.is_empty() {
                ambient.insert(wire.clone(), slot);
            }
        }
    }
    let id_encoding = if id_field == "url" {
        IdEncoding::Url
    } else if !compound_key_vars.is_empty() {
        IdEncoding::Compound
    } else {
        IdEncoding::Simple
    };
    RowIdentity::new(qualified_entity, reference, ambient, id_encoding)
}

/// Build row identity from a live HTTP row reference and optional ambient scope slots.
pub fn row_identity_from_ref(
    qualified_entity: QualifiedEntityKey,
    reference: Ref,
    ambient: IndexMap<String, String>,
    id_encoding: IdEncoding,
) -> RowIdentity {
    RowIdentity::new(qualified_entity, reference, ambient, id_encoding)
}

/// Resolve relation target [`Ref`] from a canonical row identity (shared by runtime chain GET and plan holes).
pub fn resolve_relation_target_id(
    source: &RowIdentity,
    relation_wire: &str,
    target_ent: &crate::schema::EntityDef,
) -> Result<Ref, String> {
    if let Some(v) = source.ambient.get(relation_wire) {
        if !v.is_empty() {
            return Ok(Ref::new(target_ent.name.clone(), v.clone()));
        }
    }
    Err(format!(
        "relation `{relation_wire}` has no target id on row for entity `{}`",
        source.qualified_entity.entity
    ))
}

/// Documented suffix-stream peel for postfix transforms only (relation hops require CGS-aware decompose in agent-core).
pub fn parse_row_suffix_stream_tail(expr: &str) -> Result<(String, Vec<RowSuffix>), String> {
    use crate::expr_parser::postfix::peel_postfix_suffixes;
    let (core, ops) = peel_postfix_suffixes(expr)?;
    let suffixes = ops
        .iter()
        .map(RowSuffix::from_postfix_op)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((core, suffixes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::EntityDef;

    #[test]
    fn row_identity_from_parts_populates_relation_ambient_and_url_encoding() {
        let mut relations = IndexMap::new();
        relations.insert(
            "evolution_chain".into(),
            vec![Ref::new(
                "EvolutionChain",
                "https://pokeapi.co/api/v2/evolution-chain/10/",
            )],
        );
        let identity = row_identity_from_parts(
            QualifiedEntityKey::new("pokeapi".to_string(), "PokemonSpecies".to_string()),
            Ref::new("PokemonSpecies", "bulbasaur"),
            &relations,
            "name",
            &[],
        );
        assert_eq!(identity.id_encoding, IdEncoding::Simple);
        assert_eq!(
            identity.ambient.get("evolution_chain").map(String::as_str),
            Some("https://pokeapi.co/api/v2/evolution-chain/10/")
        );
        let target = EntityDef {
            name: "EvolutionChain".into(),
            description: String::new(),
            id_field: "url".into(),
            id_format: None,
            id_from: None,
            fields: IndexMap::new(),
            relations: IndexMap::new(),
            expression_aliases: Vec::new(),
            implicit_request_identity: false,
            key_vars: Vec::new(),
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        };
        let target_ref =
            resolve_relation_target_id(&identity, "evolution_chain", &target).expect("resolve");
        assert_eq!(
            target_ref.primary_slot_str(),
            "https://pokeapi.co/api/v2/evolution-chain/10/"
        );
    }

    #[test]
    fn resolve_relation_target_id_errors_without_ambient_slot() {
        let identity = row_identity_from_parts(
            QualifiedEntityKey::new("pokeapi".to_string(), "PokemonSpecies".to_string()),
            Ref::new("PokemonSpecies", "pikachu"),
            &IndexMap::new(),
            "name",
            &[],
        );
        let target = EntityDef {
            name: "EvolutionChain".into(),
            description: String::new(),
            id_field: "url".into(),
            id_format: None,
            id_from: None,
            fields: IndexMap::new(),
            relations: IndexMap::new(),
            expression_aliases: Vec::new(),
            implicit_request_identity: false,
            key_vars: Vec::new(),
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        };
        assert!(resolve_relation_target_id(&identity, "evolution_chain", &target).is_err());
    }

    #[test]
    fn row_identity_from_parts_url_id_field_uses_url_encoding() {
        let identity = row_identity_from_parts(
            QualifiedEntityKey::new("pokeapi".to_string(), "EvolutionChain".to_string()),
            Ref::new(
                "EvolutionChain",
                "https://pokeapi.co/api/v2/evolution-chain/10/",
            ),
            &IndexMap::new(),
            "url",
            &[],
        );
        assert_eq!(identity.id_encoding, IdEncoding::Url);
        assert_eq!(
            identity.ambient.get("url").map(String::as_str),
            Some("https://pokeapi.co/api/v2/evolution-chain/10/")
        );
    }
}
