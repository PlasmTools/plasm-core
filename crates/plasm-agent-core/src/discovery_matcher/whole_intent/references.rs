//! Literal references are supporting evidence only. An unrecognised clause
//! terminates its list; quoted text cannot introduce a command or a separator.
use super::{CapabilityRef, CGS};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct References {
    pub plain: String,
    complete: bool,
    keys: BTreeSet<CapabilityRef>,
    pub lists: Vec<BTreeSet<CapabilityRef>>,
}

/// Preserve quoted arguments verbatim while keeping them out of instruction
/// matching. The search packet binds each marker to its original text.
pub(crate) struct IntentText<'a> {
    pub outer: std::borrow::Cow<'a, str>,
    pub literals: Vec<&'a str>,
}

impl<'a> IntentText<'a> {
    pub fn parse(intent: &'a str) -> Self {
        let mut outer = String::new();
        let mut literals = Vec::new();
        let mut closing = None;
        let mut start = 0;
        let mut escaped = false;
        let mut previous = None::<char>;
        for (i, c) in intent.char_indices() {
            if let Some(end) = closing {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == end {
                    literals.push(&intent[start..i]);
                    closing = None;
                    start = i + c.len_utf8();
                }
            } else {
                let end = match c {
                    '"' | '`' => Some(c),
                    '\'' if !previous.is_some_and(char::is_alphanumeric) => Some(c),
                    '“' => Some('”'),
                    '‘' => Some('’'),
                    _ => None,
                };
                if let Some(end) = end {
                    outer.push_str(&intent[start..i]);
                    use std::fmt::Write;
                    write!(outer, "⟦literal{}⟧", literals.len() + 1).expect("string write");
                    start = i + c.len_utf8();
                    closing = Some(end);
                }
            }
            previous = Some(c);
        }
        if closing.is_some() {
            literals.push(&intent[start..]);
        } else if literals.is_empty() {
            return Self {
                outer: intent.into(),
                literals,
            };
        } else {
            outer.push_str(&intent[start..]);
        }
        Self {
            outer: outer.into(),
            literals,
        }
    }
}

impl References {
    pub fn bind(intent: &str, catalogs: &BTreeMap<String, CGS>) -> Self {
        let plain = IntentText::parse(intent).outer.to_lowercase();
        // Scope-changing instructions require semantic interpretation. Leave
        // them to the provider rather than inferring an affirmative literal.
        if plain.split(|c: char| !c.is_alphanumeric()).any(|word| {
            matches!(
                word,
                "no" | "not"
                    | "never"
                    | "except"
                    | "excluding"
                    | "exclude"
                    | "skip"
                    | "ignore"
                    | "without"
                    | "omit"
                    | "excepting"
                    | "avoid"
                    | "only"
                    | "if"
                    | "unless"
            )
        }) {
            return Self {
                plain: String::new(),
                complete: false,
                keys: BTreeSet::new(),
                lists: Vec::new(),
            };
        }
        let mut lists = Vec::new();
        let mut complete = true;
        let mut keys = BTreeSet::new();
        let lookup_text = plain.trim_end_matches(['.', ' ']);
        let lookup_text = lookup_text
            .strip_suffix(". return their records")
            .unwrap_or(lookup_text);
        for sentence in lookup_text.split(['.', ';', '!', '?', '\n']) {
            let mut list = BTreeSet::new();
            let mut active = false;
            for clause in sentence
                .split(',')
                .flat_map(|s| s.split(" and "))
                .flat_map(|s| s.split(" plus "))
            {
                let clause = clause.trim();
                if clause.is_empty() {
                    continue;
                }
                let direct = ["read ", "get ", "fetch ", "retrieve ", "show ", "look up "]
                    .iter()
                    .find_map(|verb| clause.strip_prefix(verb));
                if active || direct.is_some() {
                    let phrase = direct.unwrap_or(clause).trim_start();
                    if let Some(reference) =
                        bind_key(phrase.strip_prefix("the ").unwrap_or(phrase), catalogs)
                    {
                        keys.insert(reference.clone());
                        list.insert(reference);
                        active = true;
                        continue;
                    }
                }
                complete = false;
                finish_list(&mut list, &mut lists);
                active = false;
            }
            finish_list(&mut list, &mut lists);
        }
        Self {
            plain,
            lists,
            complete,
            keys,
        }
    }

    /// Only constrain recall when every clause binds a catalogue identity.
    /// Keep alternative reads for the same record type available to the selector.
    pub(crate) fn record_reads(
        intent: &str,
        catalogs: &BTreeMap<String, CGS>,
    ) -> Option<BTreeSet<CapabilityRef>> {
        // A final projection adds no operation when the bound lookups already
        // provide every field. Keep an unrecognized clause in whole-intent discovery.
        let plain = intent.trim().trim_end_matches('.').to_lowercase();
        let projection = plain
            .rsplit_once(" and return ")
            .or_else(|| plain.rsplit_once(". return "));
        let (lookup, fields) = match projection {
            Some((lookup, fields)) if fields != "their records" => {
                let fields = fields
                    .strip_prefix("its ")
                    .or_else(|| fields.strip_prefix("their "))
                    .unwrap_or(fields);
                (lookup, Some(fields))
            }
            _ => (intent, None),
        };
        let bound = Self::bind(lookup, catalogs);
        if !bound.complete || bound.keys.is_empty() {
            return None;
        }
        let mut reads = BTreeSet::new();
        for reference in &bound.keys {
            let cgs = &catalogs[&reference.catalog];
            let getter = &cgs.capabilities[reference.capability.as_str()];
            let domain = &getter.domain;
            if let Some(fields) = fields {
                let entity = &cgs.entities[domain.as_str()];
                let supplied = fields
                    .split(',')
                    .flat_map(|s| s.split(" and "))
                    .map(str::trim)
                    .all(|field| {
                        entity.fields.contains_key(field)
                            && getter.provides.iter().any(|p| p == field)
                    });
                if !supplied {
                    return None;
                }
                reads.insert(reference.clone());
                continue;
            }
            for cap in cgs.capabilities.values().filter(|cap| {
                &cap.domain == domain
                    && matches!(
                        cap.kind,
                        plasm_core::CapabilityKind::Get
                            | plasm_core::CapabilityKind::Query
                            | plasm_core::CapabilityKind::Search
                    )
            }) {
                reads.insert(CapabilityRef {
                    catalog: reference.catalog.clone(),
                    capability: cap.name.to_string(),
                });
            }
        }
        Some(reads)
    }
}

fn finish_list(list: &mut BTreeSet<CapabilityRef>, output: &mut Vec<BTreeSet<CapabilityRef>>) {
    if list
        .iter()
        .map(|r| &r.catalog)
        .collect::<BTreeSet<_>>()
        .len()
        >= 2
    {
        output.push(std::mem::take(list));
    } else {
        list.clear();
    }
}

/// A keyed lookup requires a pinned transport binding to the record identifier.
pub(super) fn consumes_record_key(
    cap: &plasm_core::schema::CapabilitySchema,
    entity: &plasm_core::schema::EntityDef,
) -> bool {
    if cap.kind != plasm_core::CapabilityKind::Get || cap.domain != entity.name {
        return false;
    }
    let Some(mapping) = &cap.mapping else {
        return false;
    };
    let vars = plasm_core::path_env::identity_env_var_names(&mapping.template.0);
    if cap
        .input_fields()
        .filter(|field| field.required)
        .any(|field| !vars.as_slice().iter().any(|var| var == &field.name))
    {
        return false;
    }
    if entity.key_vars.len() > 1 {
        return vars.len() == entity.key_vars.len()
            && entity
                .key_vars
                .iter()
                .all(|k| vars.as_slice().iter().any(|v| v == k.as_str()));
    }
    matches!(vars.as_slice(), [var] if plasm_core::path_env::is_identity_projectable(
        entity, &vars, var, plasm_core::path_env::SoleAliasPolicy::AllowedOnSimpleKey))
}

/// Resolve the complete catalogue and noun phrase against authored names.
/// Exactly one pinned identity-consuming Get must match the clause.
fn bind_key(phrase: &str, catalogs: &BTreeMap<String, CGS>) -> Option<CapabilityRef> {
    let (phrase, key_field) = phrase
        .rsplit_once(" by ")
        .map_or((phrase, None), |(s, f)| (s, Some(f)));
    let (prefix, key) = phrase.rsplit_once(char::is_whitespace)?;
    let components: Vec<_> = key.split('/').collect();
    if components.len() > 2
        || !components.iter().all(|part| {
            part.starts_with(char::is_alphanumeric)
                && part
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
        })
    {
        return None;
    }
    let prefix = prefix
        .trim_end()
        .strip_suffix(" of")
        .unwrap_or(prefix.trim_end());
    let mut found = None;
    for (catalog_name, catalog) in catalogs {
        for (offset, _) in prefix.match_indices(char::is_whitespace) {
            if !prefix[..offset]
                .split_whitespace()
                .collect::<String>()
                .eq_ignore_ascii_case(catalog_name)
            {
                continue;
            }
            let noun = prefix[offset..].trim_start();
            for entity in catalog.entities.values() {
                if !entity.name.as_str().eq_ignore_ascii_case(noun)
                    && !entity
                        .discovery
                        .iter()
                        .flat_map(|h| &h.names)
                        .any(|n| n.eq_ignore_ascii_case(noun))
                {
                    continue;
                }
                let is_text_key = |name: &str| {
                    entity
                        .fields
                        .get(name)
                        .and_then(|f| catalog.named_value_for_slot(f).ok())
                        .is_some_and(|slot| slot.field_type == plasm_core::FieldType::String)
                };
                // Match the language's two-part slash identity only for a
                // catalogue declaring both components. Its display ID may be numeric.
                let bound = match components.as_slice() {
                    [_, _] => {
                        key_field.is_none()
                            && entity.key_vars.len() == 2
                            && entity
                                .key_vars
                                .iter()
                                .all(|field| is_text_key(field.as_str()))
                    }
                    [_] => {
                        entity.key_vars.len() <= 1
                            && is_text_key(entity.id_field.as_str())
                            && key_field.is_none_or(|field| {
                                entity.id_field.as_str().eq_ignore_ascii_case(field)
                            })
                    }
                    _ => false,
                };
                if !bound {
                    continue;
                }
                for cap in catalog
                    .capabilities
                    .values()
                    .filter(|cap| consumes_record_key(cap, entity))
                {
                    if found.is_some() {
                        return None;
                    }
                    found = Some(CapabilityRef {
                        catalog: catalog_name.clone(),
                        capability: cap.name.to_string(),
                    });
                }
            }
        }
    }
    found
}
