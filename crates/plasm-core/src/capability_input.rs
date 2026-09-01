//! Capability input validation — shared by invoke/create type-check and predicate paths.

use crate::array_field_policy::ArrayFieldCoercionPolicy;
use crate::entity_ref_value::normalize_entity_ref_value_for_target;
use crate::{ArrayItemsSchema, FieldType, TypeError, Value, CGS};

/// Human-facing “what to write instead of `$`” for LLM corrections.
fn expected_type_phrase_for_placeholder(field_type: &FieldType) -> String {
    match field_type {
        FieldType::EntityRef { target, .. } => format!(
            "a real id or reference for `{target}` (`$` in examples is only a stand-in, not a wire value)"
        ),
        FieldType::Uuid => {
            "a UUID string in standard form — never the literal `$`".into()
        }
        FieldType::String | FieldType::Date => {
            "a concrete string for this slot (quotes if needed) — never the literal `$`".into()
        }
        FieldType::Blob => {
            "a base64 or attachment-shaped value for this slot — never the literal `$`".into()
        }
        FieldType::Integer => "a concrete integer — never the literal `$`".into(),
        FieldType::Number | FieldType::Money => "a concrete number — never the literal `$`".into(),
        FieldType::Boolean => "`true` or `false` — never `$`".into(),
        FieldType::Select => {
            "one of the allowed values the schema lists for this field — never `$`".into()
        }
        FieldType::Array | FieldType::MultiSelect | FieldType::Json => format!(
            "a value matching {:?} for this slot — never the literal `$`",
            field_type
        ),
    }
}

/// Like [`Value::is_compatible_with_field_type`], plus target-aware normalization for
/// [`FieldType::EntityRef`] (row narrowing, `full_name` split, compound-key completeness).
pub(crate) fn value_fits_field_type_entity_ref_aware(
    value: &Value,
    field_type: &FieldType,
    cgs: &CGS,
) -> bool {
    let FieldType::EntityRef { target, .. } = field_type else {
        return value.is_compatible_with_field_type(field_type);
    };
    let Some(ent) = cgs.get_entity(target) else {
        return false;
    };
    match value {
        Value::PlasmInputRef(_) | Value::Null => true,
        _ => normalize_entity_ref_value_for_target(value, ent).is_some(),
    }
}

fn entity_ref_predicate_hint(target_name: &str, cgs: &CGS) -> String {
    let Some(ent) = cgs.get_entity(target_name) else {
        return format!("EntityRef({target_name})");
    };
    let keys = if ent.key_vars.is_empty() {
        ent.id_field.as_str().to_string()
    } else {
        ent.key_vars
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "EntityRef({target_name}) — expected an entity reference or scalar identity ({keys}); use a teaching constructor, `anchor.<relation>`, or those identity fields — values that look like full entity rows without extractable scalars for those slots are not accepted here"
    )
}

pub(crate) fn entity_ref_incompatible_value(
    field_name: &str,
    target_name: &str,
    value: &Value,
    cgs: &CGS,
) -> TypeError {
    TypeError::IncompatibleValue {
        field: field_name.to_string(),
        value_type: value.type_name().to_string(),
        field_type: entity_ref_predicate_hint(target_name, cgs),
    }
}

pub(crate) fn domain_placeholder_literal_error(
    field: impl Into<String>,
    field_type: &FieldType,
    description: Option<&str>,
) -> TypeError {
    let description = description
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    TypeError::DomainPlaceholderLiteral {
        field: field.into(),
        expected_type: expected_type_phrase_for_placeholder(field_type),
        description,
    }
}

fn validate_array_item_value(
    value: &Value,
    spec: &ArrayItemsSchema,
    path: &str,
    cgs: &CGS,
) -> Result<(), TypeError> {
    if value.is_domain_example_placeholder() {
        return Ok(());
    }
    if !value_fits_field_type_entity_ref_aware(value, &spec.field_type, cgs) {
        return Err(TypeError::IncompatibleValue {
            field: path.to_string(),
            value_type: value.type_name().to_string(),
            field_type: format!("{:?}", spec.field_type),
        });
    }
    if matches!(spec.field_type, FieldType::Select) {
        if let (Some(allowed), Some(sv)) = (&spec.allowed_values, value.as_str()) {
            if !allowed.contains(&sv.to_string()) {
                return Err(TypeError::IncompatibleValue {
                    field: path.to_string(),
                    value_type: format!("'{sv}' (not in allowed values)"),
                    field_type: format!("select with values: {:?}", allowed),
                });
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_typed_array_value(
    value: &Value,
    spec: &ArrayItemsSchema,
    path: &str,
    cgs: &CGS,
) -> Result<(), TypeError> {
    if ArrayFieldCoercionPolicy::accepts_deferred_value(value) {
        return Ok(());
    }
    let Some(arr) = value.as_array() else {
        return Err(TypeError::IncompatibleValue {
            field: path.to_string(),
            value_type: value.type_name().to_string(),
            field_type: "array".to_string(),
        });
    };
    for (i, el) in arr.iter().enumerate() {
        validate_array_item_value(el, spec, &format!("{path}[{i}]"), cgs)?;
    }
    Ok(())
}

pub(crate) fn validate_multiselect_value(
    value: &Value,
    allowed: &[String],
    path: &str,
) -> Result<(), TypeError> {
    let Some(arr) = value.as_array() else {
        return Err(TypeError::IncompatibleValue {
            field: path.to_string(),
            value_type: value.type_name().to_string(),
            field_type: "multi_select (array)".to_string(),
        });
    };
    for (i, el) in arr.iter().enumerate() {
        if el.is_domain_example_placeholder() {
            continue;
        }
        let Some(sv) = el.as_str() else {
            return Err(TypeError::IncompatibleValue {
                field: format!("{path}[{i}]"),
                value_type: el.type_name().to_string(),
                field_type: "multi_select element (expected string)".to_string(),
            });
        };
        if !allowed.contains(&sv.to_string()) {
            return Err(TypeError::IncompatibleValue {
                field: format!("{path}[{i}]"),
                value_type: format!("'{sv}' (not in allowed values)"),
                field_type: format!("multi_select with values: {:?}", allowed),
            });
        }
    }
    Ok(())
}

/// Validate a concrete value against a resolved [`NamedValueSchema`] (invoke params + predicates).
pub(crate) fn validate_concrete_named_value(
    value: &Value,
    nv: &crate::NamedValueSchema,
    field_path: &str,
    cgs: &CGS,
) -> Result<(), TypeError> {
    match &nv.field_type {
        FieldType::Array => {
            let spec = nv
                .array_items
                .as_ref()
                .ok_or_else(|| TypeError::IncompatibleValue {
                    field: field_path.to_string(),
                    value_type: value.type_name().to_string(),
                    field_type: "array (missing items schema)".to_string(),
                })?;
            validate_typed_array_value(value, spec, field_path, cgs)
        }
        FieldType::MultiSelect => {
            let allowed = nv.allowed_values.as_deref().unwrap_or(&[]);
            validate_multiselect_value(value, allowed, field_path)
        }
        _ => {
            if !value_fits_field_type_entity_ref_aware(value, &nv.field_type, cgs) {
                return Err(match &nv.field_type {
                    FieldType::EntityRef { target, .. } => {
                        entity_ref_incompatible_value(field_path, target.as_str(), value, cgs)
                    }
                    _ => TypeError::IncompatibleValue {
                        field: field_path.to_string(),
                        value_type: value.type_name().to_string(),
                        field_type: format!("{:?}", nv.field_type),
                    },
                });
            }
            if matches!(nv.field_type, FieldType::Money) {
                validate_money_named_value(field_path, value, nv)?;
            }
            validate_named_value_domain(value, nv, field_path)
        }
    }
}

fn validate_money_named_value(
    field_path: &str,
    value: &Value,
    nv: &crate::NamedValueSchema,
) -> Result<(), TypeError> {
    let fmt = crate::money::MoneyWireFormat::DecimalString;
    let coerced =
        crate::money::normalize(value.clone(), fmt, nv.currency.as_deref()).map_err(|message| {
            TypeError::IncompatibleValue {
                field: field_path.to_string(),
                value_type: format!("{} ({message})", value.type_name()),
                field_type: "money".to_string(),
            }
        })?;
    let Value::Money(m) = coerced else {
        return Ok(());
    };
    crate::money::currency_conflict(nv.currency.as_deref(), m.currency()).map_err(TypeError::from)
}

/// Validate input against capability input schema
pub(crate) fn validate_capability_input(
    input: &Value,
    input_schema: &crate::InputSchema,
    cgs: &CGS,
) -> Result<(), TypeError> {
    validate_input_type(input, &input_schema.input_type, "", cgs)?;
    validate_input_constraints(input, &input_schema.validation)?;
    Ok(())
}

/// Validate a value against an input type specification
pub(crate) fn validate_input_type(
    value: &Value,
    input_type: &crate::InputType,
    path: &str,
    cgs: &CGS,
) -> Result<(), TypeError> {
    let path_label = || {
        if path.is_empty() {
            "input".to_string()
        } else {
            path.to_string()
        }
    };

    if ArrayFieldCoercionPolicy::accepts_deferred_value(value) {
        return Ok(());
    }

    match input_type {
        crate::InputType::None => {
            if value.is_domain_example_placeholder() {
                return Err(TypeError::DomainPlaceholderLiteral {
                    field: path_label(),
                    expected_type: "this action expects no request body — remove `$` entirely"
                        .into(),
                    description: None,
                });
            }
            if !matches!(value, Value::Null) {
                return Err(TypeError::IncompatibleValue {
                    field: path.to_string(),
                    value_type: value.type_name().to_string(),
                    field_type: "none (no input expected)".to_string(),
                });
            }
        }

        crate::InputType::Value {
            field_type,
            allowed_values,
        } => {
            if value.is_domain_example_placeholder() {
                return Err(domain_placeholder_literal_error(
                    path_label(),
                    field_type,
                    None,
                ));
            }
            if !value_fits_field_type_entity_ref_aware(value, field_type, cgs) {
                let lbl = path_label();
                return Err(match field_type {
                    FieldType::EntityRef { target, .. } => {
                        entity_ref_incompatible_value(lbl.as_str(), target.as_str(), value, cgs)
                    }
                    _ => TypeError::IncompatibleValue {
                        field: path.to_string(),
                        value_type: value.type_name().to_string(),
                        field_type: format!("{:?}", field_type),
                    },
                });
            }

            // Check allowed values for select types
            if let (Some(allowed), Some(string_val)) = (allowed_values, value.as_str()) {
                if !allowed.contains(&string_val.to_string()) {
                    return Err(TypeError::IncompatibleValue {
                        field: path.to_string(),
                        value_type: format!("'{}' (not in allowed values)", string_val),
                        field_type: format!("select with values: {:?}", allowed),
                    });
                }
            }
        }

        crate::InputType::Object {
            fields,
            additional_fields,
        } => {
            if value.is_domain_example_placeholder() {
                return Err(TypeError::DomainPlaceholderLiteral {
                    field: path_label(),
                    expected_type: "an object with the fields the prompt lists for this action (e.g. `{name: …}`) — never the bare `$` token".into(),
                    description: None,
                });
            }
            let Some(object) = value.as_object() else {
                return Err(TypeError::IncompatibleValue {
                    field: path.to_string(),
                    value_type: value.type_name().to_string(),
                    field_type: "object".to_string(),
                });
            };

            // Validate required fields
            for field_schema in fields {
                let field_path = if path.is_empty() {
                    field_schema.name.clone()
                } else {
                    format!("{}.{}", path, field_schema.name)
                };

                match object.get(&field_schema.name) {
                    Some(field_value) => {
                        if !field_value.is_domain_example_placeholder() {
                            match &field_schema.wire {
                                crate::InputFieldWire::Inline(ty) => {
                                    validate_input_type(
                                        field_value,
                                        ty.as_ref(),
                                        &field_path,
                                        cgs,
                                    )?;
                                }
                                crate::InputFieldWire::Registry(_) => {
                                    let fnv = field_schema.named_value(cgs).map_err(|_| {
                                        TypeError::FieldNotFound {
                                            field: field_path.clone(),
                                            entity: "input object".to_string(),
                                        }
                                    })?;
                                    validate_concrete_named_value(
                                        field_value,
                                        fnv,
                                        &field_path,
                                        cgs,
                                    )?;
                                }
                            }
                        }
                    }
                    None => {
                        if field_schema.required {
                            return Err(TypeError::FieldNotFound {
                                field: field_path,
                                entity: "input object".to_string(),
                            });
                        }
                    }
                }
            }

            // Check for unexpected fields if additional_fields is false
            if !additional_fields {
                let defined_fields: std::collections::HashSet<_> =
                    fields.iter().map(|f| &f.name).collect();

                for object_field in object.keys() {
                    if !defined_fields.contains(object_field) {
                        return Err(TypeError::FieldNotFound {
                            field: format!("{}.{}", path, object_field),
                            entity: "additional fields not allowed".to_string(),
                        });
                    }
                }
            }
        }

        crate::InputType::Array {
            element_type,
            min_length,
            max_length,
        } => {
            if value.is_domain_example_placeholder() {
                return Err(TypeError::DomainPlaceholderLiteral {
                    field: path_label(),
                    expected_type: "an array `[...]` as the examples show for this parameter — not the bare `$` token".into(),
                    description: None,
                });
            }
            let Some(array) = value.as_array() else {
                return Err(TypeError::IncompatibleValue {
                    field: path.to_string(),
                    value_type: value.type_name().to_string(),
                    field_type: "array".to_string(),
                });
            };

            // Check length constraints
            if let Some(min) = min_length {
                if array.len() < *min {
                    return Err(TypeError::IncompatibleValue {
                        field: path.to_string(),
                        value_type: format!("array of length {}", array.len()),
                        field_type: format!("array with min length {}", min),
                    });
                }
            }

            if let Some(max) = max_length {
                if array.len() > *max {
                    return Err(TypeError::IncompatibleValue {
                        field: path.to_string(),
                        value_type: format!("array of length {}", array.len()),
                        field_type: format!("array with max length {}", max),
                    });
                }
            }

            // Validate each element
            for (i, element) in array.iter().enumerate() {
                let element_path = format!("{}[{}]", path, i);
                validate_input_type(element, element_type, &element_path, cgs)?;
            }
        }

        crate::InputType::Union { variants } => {
            if value.is_domain_example_placeholder() {
                // teaching table dotted-call teaching uses `$` / `[$]` as fill-ins; union-shaped invoke slots
                // (e.g. edit/v2 `operations` rows) share the same placeholder convention as scalar params.
                return Ok(());
            }
            if let Value::UnionCtor {
                ctor_label,
                ctor_fields,
            } = value
            {
                let Some(variant) = variants.iter().find(|v| {
                    crate::schema::union_variant_constructor_symbol(v) == Some(ctor_label.as_str())
                }) else {
                    return Err(TypeError::IncompatibleValue {
                        field: path.to_string(),
                        value_type: format!("unknown union constructor `{ctor_label}`"),
                        field_type: format!("union of {} variants", variants.len()),
                    });
                };
                let body_ty = crate::schema::input_variant_body_type(variant);
                return validate_input_type(
                    &Value::Object(ctor_fields.clone()),
                    &body_ty,
                    path,
                    cgs,
                );
            }
            if let Value::Object(obj) = value {
                for variant in variants {
                    let wf = variant.wire.field.as_str();
                    if let Some(Value::String(disc)) = obj.get(wf) {
                        if disc.as_str() == variant.wire.value.as_str() {
                            let mut stripped = obj.clone();
                            stripped.shift_remove(wf);
                            let body_ty = crate::schema::input_variant_body_type(variant);
                            let logical_val =
                                if crate::typed_invoke::union_variant_needs_wire_decode(variant) {
                                    match crate::typed_invoke::logical_object_from_wire_union_body(
                                        &stripped, variant,
                                    ) {
                                        Ok(v) => v,
                                        Err(()) => {
                                            return Err(TypeError::IncompatibleValue {
                                                field: path.to_string(),
                                                value_type: "object".into(),
                                                field_type: format!(
                                                    "union variant `{}` wire body decode failed",
                                                    variant.name
                                                ),
                                            });
                                        }
                                    }
                                } else {
                                    Value::Object(stripped)
                                };
                            return validate_input_type(&logical_val, &body_ty, path, cgs);
                        }
                    }
                }
            }

            return Err(TypeError::IncompatibleValue {
                field: path.to_string(),
                value_type: value.type_name().to_string(),
                field_type: format!("union of {} variants", variants.len()),
            });
        }
    }

    Ok(())
}

/// Validate profile + constraints on a concrete scalar value (shared by compile and decode).
pub(crate) fn validate_named_value_domain_value(
    value: &Value,
    nv: &crate::NamedValueSchema,
) -> Result<(), String> {
    if value.is_domain_example_placeholder() {
        return Ok(());
    }
    if let Some(s) = value.as_str() {
        nv.domain.validate_string_value(s)
    } else if let Some(n) = value.as_number() {
        nv.domain.validate_number_value(n)
    } else {
        Ok(())
    }
}

/// Validate profile + constraints on a concrete invoke/compile value.
pub(crate) fn validate_named_value_domain(
    value: &Value,
    nv: &crate::NamedValueSchema,
    field_path: &str,
) -> Result<(), TypeError> {
    validate_named_value_domain_value(value, nv).map_err(|msg| {
        let value_type = if let Some(s) = value.as_str() {
            format!("'{s}'")
        } else if let Some(n) = value.as_number() {
            n.to_string()
        } else {
            value.type_name().to_string()
        };
        TypeError::IncompatibleValue {
            field: field_path.to_string(),
            value_type,
            field_type: msg,
        }
    })
}

/// Validate input constraints
fn validate_input_constraints(
    input: &Value,
    validation: &crate::InputValidation,
) -> Result<(), TypeError> {
    // Check null allowance
    if matches!(input, Value::Null) && !validation.allow_null {
        return Err(TypeError::IncompatibleValue {
            field: "input".to_string(),
            value_type: "null".to_string(),
            field_type: "non-null value required".to_string(),
        });
    }

    // Apply cross-field rules for object inputs
    if let Value::Object(obj) = input {
        for rule in &validation.cross_field_rules {
            validate_cross_field_rule(obj, rule)?;
        }
    }

    Ok(())
}

/// Validate cross-field rules
fn validate_cross_field_rule(
    object: &indexmap::IndexMap<String, Value>,
    rule: &crate::CrossFieldRule,
) -> Result<(), TypeError> {
    // Teaching-surface `$` placeholders count as absent (same spirit as value-domain constraints).
    // When every listed field is absent or still a placeholder, defer the rule to execute time.
    let concretely_present: Vec<_> = rule
        .fields
        .iter()
        .filter(|&field| match object.get(field) {
            None | Some(Value::Null) => false,
            Some(v) if v.is_domain_example_placeholder() => false,
            Some(_) => true,
        })
        .collect();
    let any_placeholder = rule
        .fields
        .iter()
        .any(|field| matches!(object.get(field), Some(v) if v.is_domain_example_placeholder()));
    if concretely_present.is_empty() && any_placeholder {
        return Ok(());
    }

    let present_fields = concretely_present;

    let valid = match rule.rule_type {
        crate::CrossFieldRuleType::AtLeastOne => !present_fields.is_empty(),
        crate::CrossFieldRuleType::ExactlyOne => present_fields.len() == 1,
        crate::CrossFieldRuleType::AllOrNone => {
            present_fields.is_empty() || present_fields.len() == rule.fields.len()
        }
        crate::CrossFieldRuleType::Implies => {
            // If first field is concretely present, second must be too
            if rule.fields.len() >= 2 {
                let first_present = present_fields.iter().any(|f| *f == &rule.fields[0]);
                let second_present = present_fields.iter().any(|f| *f == &rule.fields[1]);
                !first_present || second_present
            } else {
                true
            }
        }
        crate::CrossFieldRuleType::MutuallyExclusive => present_fields.len() <= 1,
    };

    if !valid {
        return Err(TypeError::IncompatibleValue {
            field: rule.fields.join(", "),
            value_type: format!("fields present: {:?}", present_fields),
            field_type: rule.error_message.clone(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value_domain::{Constraints, KernelKind, ValueDomain};
    use crate::Value;
    use indexmap::IndexMap;

    fn revenue_nv_min_zero() -> crate::NamedValueSchema {
        crate::NamedValueSchema::from_domain(
            String::new(),
            ValueDomain::new(
                KernelKind::Number,
                None,
                Constraints {
                    min: Some(0.0),
                    ..Default::default()
                },
                None,
                None,
            )
            .expect("number domain"),
            None,
        )
    }

    fn obj(entries: &[(&str, Value)]) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in entries {
            m.insert((*k).to_string(), v.clone());
        }
        Value::Object(m)
    }

    /// WS-R3′: the teaching-surface `$` fill-in is not a real API value; value-domain enforcement is
    /// deferred to execute time rather than rejecting the teaching line.
    #[test]
    fn value_domain_on_domain_placeholder_is_deferred() {
        let nv = revenue_nv_min_zero();
        validate_named_value_domain(&Value::String("$".to_string()), &nv, "revenue")
            .expect("`$` placeholder must skip value-domain constraints");
    }

    /// A concrete violating value is still rejected via `values:` constraints.
    #[test]
    fn value_domain_on_concrete_violation_still_fails() {
        let nv = revenue_nv_min_zero();
        validate_named_value_domain(&Value::Integer(-5), &nv, "revenue")
            .expect_err("a real negative revenue must still fail min constraint");
    }

    /// GitHub FO: `pr_create` ExactlyOne(title, issue) — title+issue must fail at plan/typecheck.
    #[test]
    fn github_pr_create_rejects_title_and_issue_together() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
        let cgs = crate::loader::load_schema_dir(&dir).expect("github catalog");
        let cap = cgs.capabilities.get("pr_create").expect("pr_create");
        let schema = cap.input_schema.as_ref().expect("pr_create input_schema");
        assert!(
            schema
                .validation
                .cross_field_rules
                .iter()
                .any(|r| r.rule_type == crate::CrossFieldRuleType::ExactlyOne
                    && r.fields.iter().any(|f| f == "title")
                    && r.fields.iter().any(|f| f == "issue")),
            "pr_create must stamp exactly_one title|issue"
        );
        let both = obj(&[
            ("repository", Value::String("o/r".into())),
            ("title", Value::String("t".into())),
            ("head", Value::String("h".into())),
            ("base", Value::String("main".into())),
            ("issue", Value::Integer(1)),
        ]);
        let err = validate_capability_input(&both, schema, &cgs)
            .expect_err("title+issue must fail exactly_one");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("exactly one") || msg.contains("title") || msg.contains("issue"),
            "error should name the XOR: {msg}"
        );
        let title_only = obj(&[
            ("repository", Value::String("o/r".into())),
            ("title", Value::String("t".into())),
            ("head", Value::String("h".into())),
            ("base", Value::String("main".into())),
        ]);
        validate_capability_input(&title_only, schema, &cgs).expect("title-only ok");
        let issue_only = obj(&[
            ("repository", Value::String("o/r".into())),
            ("issue", Value::Integer(1)),
            ("head", Value::String("h".into())),
            ("base", Value::String("main".into())),
        ]);
        validate_capability_input(&issue_only, schema, &cgs).expect("issue-only ok");
        // `$` is absent: title alone remains exactly_one-valid (not “any `$` ⇒ skip rule”).
        let title_and_placeholder_issue = obj(&[
            ("repository", Value::String("o/r".into())),
            ("title", Value::String("t".into())),
            ("head", Value::String("h".into())),
            ("base", Value::String("main".into())),
            ("issue", Value::String("$".into())),
        ]);
        validate_capability_input(&title_and_placeholder_issue, schema, &cgs)
            .expect("title + issue=$ must treat $ as absent");
        let both_placeholders = obj(&[
            ("repository", Value::String("o/r".into())),
            ("title", Value::String("$".into())),
            ("head", Value::String("h".into())),
            ("base", Value::String("main".into())),
            ("issue", Value::String("$".into())),
        ]);
        validate_capability_input(&both_placeholders, schema, &cgs)
            .expect("all-$ fields must vacate exactly_one until execute");
    }
}
