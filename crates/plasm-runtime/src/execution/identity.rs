//! Identity ambient strings and timestamps.

use super::*;

pub(crate) fn value_to_ambient_string(v: &Value) -> Option<String> {
    match v {
        Value::PlasmInputRef(_) | Value::StringTemplate(_) => None,
        Value::String(s) | Value::PhraseIdent(s) => Some(s.clone()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null
        | Value::Array(_)
        | Value::Object(_)
        | Value::UnionCtor { .. }
        | Value::Money(_) => None,
    }
}

/// CML env slots usable as compound-key fallbacks (string-like values only).
pub(crate) fn cml_env_to_identity_strings(env: &CmlEnv) -> IndexMap<String, String> {
    let mut out = IndexMap::new();
    for (k, v) in env.iter() {
        if let Some(s) = value_to_ambient_string(v) {
            out.insert(k.clone(), s);
        }
    }
    out
}

pub(crate) fn ref_to_identity_ambient(reference: &Ref) -> IndexMap<String, String> {
    match &reference.key {
        EntityKey::Simple(_) => IndexMap::new(),
        EntityKey::Compound(parts) => parts
            .iter()
            .filter_map(|(k, v)| v.as_lit_str().map(|s| (k.clone(), s.to_string())))
            .collect(),
    }
}

pub(crate) fn decode_identity_ambient_for_ref(
    reference: &Ref,
    env: &CmlEnv,
) -> IndexMap<String, String> {
    let mut m = ref_to_identity_ambient(reference);
    for (k, v) in cml_env_to_identity_strings(env) {
        m.entry(k).or_insert(v);
    }
    m
}

/// When an update capability passes a JSON patch object as `input`, merge the entity's primary wire
/// id (`id_from`) from the resolved `id` env slot so vendors like Fibery receive `{ fibery/id, … }`.
pub(crate) fn merge_entity_id_from_into_input_env(
    env: &mut CmlEnv,
    ent: Option<&EntityDef>,
    capability: &CapabilitySchema,
) {
    if !matches!(capability.kind, CapabilityKind::Update) {
        return;
    }
    let Some(ent) = ent else {
        return;
    };
    let Some(id_from) = ent.id_from.as_ref().filter(|p| !p.is_empty()) else {
        return;
    };
    let Some(wire_key) = id_from.first() else {
        return;
    };
    let Some(Value::String(id)) = env.get("id").cloned() else {
        return;
    };
    let Some(input_val) = env.get_mut("input") else {
        return;
    };
    let Value::Object(map) = input_val else {
        return;
    };
    map.entry(wire_key.clone())
        .or_insert_with(|| Value::String(id));
}

pub(crate) fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
