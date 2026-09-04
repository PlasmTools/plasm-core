//! Identity-env var harvest and classification.

use std::collections::BTreeSet;

use crate::schema::EntityDef;
use crate::CapabilityKind;

/// Whether a sole path/GQL identity var may alias the primary key when it is not a wire name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoleAliasPolicy {
    /// Create domain proof: parent path keys must be wire names or declared inputs.
    Forbidden,
    /// Get/Update/Delete/Action domain proof, and anchor/receiver projection.
    AllowedOnSimpleKey,
}

impl CapabilityKind {
    /// Alias policy when proving path-env coverage against the capability's **domain** entity.
    #[must_use]
    pub fn domain_path_env_alias_policy(self) -> SoleAliasPolicy {
        match self {
            Self::Create => SoleAliasPolicy::Forbidden,
            _ => SoleAliasPolicy::AllowedOnSimpleKey,
        }
    }
}

impl EntityDef {
    /// True when `key_vars` is empty or a single slot (unary / simple identity).
    #[must_use]
    pub fn is_simple_keyed(&self) -> bool {
        self.key_vars.len() <= 1
    }
}

/// Ordered, deduplicated identity wire names (`id`, `id_field`, `key_vars`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IdentityWireNames(BTreeSet<String>);

impl IdentityWireNames {
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    #[must_use]
    pub fn as_sorted_vec(&self) -> Vec<String> {
        self.0.iter().cloned().collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

/// How a path / GQL identity var relates to entity identity under a policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityNameMatch {
    Wire,
    SolePathAlias,
}

/// Ordered identity-env vars: HTTP path `type: var` ∪ GraphQL operation variables.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IdentityEnvVars(Vec<String>);

impl IdentityEnvVars {
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

impl From<Vec<String>> for IdentityEnvVars {
    fn from(value: Vec<String>) -> Self {
        Self(value)
    }
}

/// Wire names [`crate::ResolvedIdentity`] may bind for `ent` (plus legacy `id`).
#[must_use]
pub fn identity_wire_names(ent: &EntityDef) -> IdentityWireNames {
    let mut names = BTreeSet::from(["id".to_string()]);
    names.insert(ent.id_field.to_string());
    for kv in &ent.key_vars {
        names.insert(kv.to_string());
    }
    IdentityWireNames(names)
}

/// HTTP path segment variable names from CML `path` (in order), `type: var` only.
#[must_use]
pub fn path_var_names_from_mapping_json(template: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(path) = template.get("path").and_then(|p| p.as_array()) else {
        return out;
    };
    for seg in path {
        if seg.get("type").and_then(|t| t.as_str()) == Some("var") {
            if let Some(name) = seg.get("name").and_then(|n| n.as_str()) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// `type: var` / `name` entries under GraphQL `body` → `variables` (operation variables only).
#[must_use]
pub fn graphql_operation_variable_names(template: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if template.get("transport").and_then(|t| t.as_str()) != Some("graphql") {
        return out;
    }
    let Some(body) = template.get("body") else {
        return out;
    };
    graphql_find_variables_block(body, &mut out);
    out.sort();
    out.dedup();
    out
}

/// Path ∪ GraphQL identity-env vars (order: path first, then GQL-only names).
#[must_use]
pub fn identity_env_var_names(template: &serde_json::Value) -> IdentityEnvVars {
    let mut vars = path_var_names_from_mapping_json(template);
    for g in graphql_operation_variable_names(template) {
        if !vars.iter().any(|v| v == &g) {
            vars.push(g);
        }
    }
    IdentityEnvVars(vars)
}

/// Classify whether `var` is identity-projectable under `policy`.
#[must_use]
pub fn classify_path_var(
    ent: &EntityDef,
    vars: &IdentityEnvVars,
    var: &str,
    policy: SoleAliasPolicy,
) -> Option<IdentityNameMatch> {
    if identity_wire_names(ent).contains(var) {
        return Some(IdentityNameMatch::Wire);
    }
    if matches!(policy, SoleAliasPolicy::AllowedOnSimpleKey)
        && vars.len() == 1
        && vars.as_slice()[0] == var
        && ent.is_simple_keyed()
    {
        return Some(IdentityNameMatch::SolePathAlias);
    }
    None
}

#[must_use]
pub fn is_identity_projectable(
    ent: &EntityDef,
    vars: &IdentityEnvVars,
    var: &str,
    policy: SoleAliasPolicy,
) -> bool {
    classify_path_var(ent, vars, var, policy).is_some()
}

fn collect_template_var_refs(template: &serde_json::Value, out: &mut Vec<String>) {
    match template {
        serde_json::Value::Object(map) => {
            if map.get("type").and_then(|t| t.as_str()) == Some("var") {
                if let Some(name) = map.get("name").and_then(|n| n.as_str()) {
                    out.push(name.to_string());
                }
            }
            for v in map.values() {
                collect_template_var_refs(v, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for e in arr {
                collect_template_var_refs(e, out);
            }
        }
        _ => {}
    }
}

/// Every CML `type: var` / `name` in the mapping template JSON (including nested bodies).
#[must_use]
pub fn capability_template_all_var_names(template: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_template_var_refs(template, &mut out);
    out.sort();
    out.dedup();
    out
}

fn graphql_find_variables_block(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(fields) = map.get("fields").and_then(|f| f.as_array()) {
                for item in fields {
                    if let Some(pair) = item.as_array() {
                        if pair.len() >= 2 {
                            let key = pair[0].as_str();
                            let val = &pair[1];
                            if key == Some("variables") {
                                collect_template_var_refs(val, out);
                                return;
                            }
                        }
                    }
                    graphql_find_variables_block(item, out);
                }
            }
            for val in map.values() {
                graphql_find_variables_block(val, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for e in arr {
                graphql_find_variables_block(e, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{EntityFieldName, EntityName};
    use indexmap::IndexMap as Im;

    fn team_entity() -> EntityDef {
        EntityDef {
            name: EntityName::from("Team"),
            description: String::new(),
            id_field: EntityFieldName::from("key"),
            id_format: None,
            id_from: None,
            fields: Im::new(),
            relations: Im::new(),
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        }
    }

    #[test]
    fn identity_wire_names_includes_id_field() {
        let ent = team_entity();
        let names = identity_wire_names(&ent);
        assert!(names.contains("id"));
        assert!(names.contains("key"));
    }

    #[test]
    fn classify_sole_alias_respects_policy() {
        let ent = team_entity();
        let vars = IdentityEnvVars(vec!["slug".into()]);
        assert_eq!(
            classify_path_var(&ent, &vars, "slug", SoleAliasPolicy::AllowedOnSimpleKey),
            Some(IdentityNameMatch::SolePathAlias)
        );
        assert_eq!(
            classify_path_var(&ent, &vars, "slug", SoleAliasPolicy::Forbidden),
            None
        );
        let multi = IdentityEnvVars(vec!["slug".into(), "extra".into()]);
        assert_eq!(
            classify_path_var(&ent, &multi, "slug", SoleAliasPolicy::AllowedOnSimpleKey),
            None
        );
    }
}
