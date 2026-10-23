use crate::value::Value;

pub(crate) fn render_surface_value(v: &Value) -> String {
    render_surface_value_named(v, &std::collections::HashMap::new())
}

pub(crate) fn render_surface_value_named(
    v: &Value,
    names: &std::collections::HashMap<String, String>,
) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Unsigned(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::String(s) | Value::PhraseIdent(s) => render_bare_or_quoted_string(s),
        Value::StringTemplate(value) => render_bare_or_quoted_string(value.source()),
        Value::PlasmInputRef(reference) => render_input_ref(reference, names),
        Value::GetScalarExtract(g) => {
            format!(
                "{}({}).{}",
                g.entity,
                render_surface_value_named(&g.identity, names),
                g.wire
            )
        }
        Value::UnionCtor {
            ctor_label,
            ctor_fields,
        } => {
            if ctor_fields.is_empty() {
                ctor_label.clone()
            } else {
                let parts: Vec<String> = ctor_fields
                    .iter()
                    .map(|(k, v)| format!("{k}={}", render_surface_value_named(v, names)))
                    .collect();
                format!("{ctor_label}{{{}}}", parts.join(", "))
            }
        }
        Value::Array(items) => {
            let inner = items
                .iter()
                .map(|v| render_surface_value_named(v, names))
                .collect::<Vec<_>>()
                .join(",");
            format!("[{inner}]")
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}={}", render_surface_value_named(v, names)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Value::Money(m) => m.display(),
    }
}

pub(crate) fn render_bare_or_quoted_string(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".to_string();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return s.to_string();
    }
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

pub(crate) fn render_id_slot(s: &str) -> String {
    if s == "$" {
        "$".to_string()
    } else if s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        s.to_string()
    } else {
        render_surface_value(&Value::String(s.to_string()))
    }
}

#[cfg(test)]
pub(crate) fn render_identity_slot(slot: &crate::IdentitySlot) -> String {
    render_identity_slot_named(slot, &std::collections::HashMap::new())
}

pub(crate) fn render_identity_slot_named(
    slot: &crate::IdentitySlot,
    names: &std::collections::HashMap<String, String>,
) -> String {
    match slot {
        crate::IdentitySlot::Lit(id) => render_id_slot(id.as_str()),
        crate::IdentitySlot::Binding(reference) => render_input_ref(reference, names),
    }
}

/// Preserve symbolic data flow without materializing an acquired value.
fn render_input_ref(
    reference: &crate::value::PlasmInputRef,
    names: &std::collections::HashMap<String, String>,
) -> String {
    use crate::value::PlasmInputRef;
    let (binding, path) = match reference {
        PlasmInputRef::NodeInput { node, path } => (names.get(node).unwrap_or(node), path),
        PlasmInputRef::RowBinding { binding, path } => (binding, path),
    };
    std::iter::once(binding.as_str())
        .chain(path.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::PlasmInputRef;

    #[test]
    fn display_names_only_rename_node_references() {
        let names = std::collections::HashMap::from([
            ("__py1".into(), "read_2".into()),
            ("_".into(), "not_a_row_cursor".into()),
        ]);
        let node = PlasmInputRef::node_output("__py1", vec!["id".into()]);
        assert_eq!(
            render_surface_value_named(&Value::PlasmInputRef(node.clone()), &names),
            "read_2.id"
        );
        assert_eq!(
            render_identity_slot_named(&crate::IdentitySlot::Binding(node), &names),
            "read_2.id"
        );
        assert_eq!(
            render_surface_value_named(
                &Value::PlasmInputRef(PlasmInputRef::row_binding("_", vec!["id".into()])),
                &names
            ),
            "_.id"
        );
        assert_eq!(
            render_surface_value_named(&Value::String("__py1.id".into()), &names),
            "\"__py1.id\""
        );
    }

    #[test]
    fn symbolic_operands_preserve_producer_field_and_row_scope() {
        for (reference, expected) in [
            (
                PlasmInputRef::node_output("acquired", vec!["id".into()]),
                "acquired.id",
            ),
            (
                PlasmInputRef::node_output("verified", vec!["id".into()]),
                "verified.id",
            ),
            (
                PlasmInputRef::node_output("acquired", vec!["token".into()]),
                "acquired.token",
            ),
            (
                PlasmInputRef::node_output("observed", vec!["location".into(), "id".into()]),
                "observed.location.id",
            ),
            (PlasmInputRef::node_output("gate", vec![]), "gate"),
            (PlasmInputRef::row_binding("_", vec!["id".into()]), "_.id"),
        ] {
            assert_eq!(
                render_surface_value(&Value::PlasmInputRef(reference.clone())),
                expected
            );
            assert_eq!(
                render_identity_slot(&crate::IdentitySlot::Binding(reference.clone())),
                expected
            );
            assert_eq!(
                render_surface_value(&Value::Array(vec![Value::PlasmInputRef(reference)])),
                format!("[{expected}]")
            );
        }
        assert_eq!(
            render_surface_value(&Value::String("acquired.id".into())),
            "\"acquired.id\""
        );
        assert_eq!(
            render_id_slot("$"),
            "$",
            "an actual placeholder must not invent provenance"
        );
    }
}
