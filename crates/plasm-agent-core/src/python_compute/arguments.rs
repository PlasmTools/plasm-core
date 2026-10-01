//! Boundary adaptation only: Monty owns the function body and Python semantics.
use super::*;
use plasm_core::value_contract::{ValueContract as Type, ValueShape as Shape};

pub(super) struct Argument {
    pub field: Option<String>,
    pub per_row: bool,
    pub value_type: Type,
}

pub(super) fn is_row(expr: &Expr) -> bool {
    name(expr) == Some("Row")
        || matches!(expr, Expr::Subscript(s) if name(&s.value).is_some_and(is_entity_record_type))
}

pub(super) fn resolve(
    annotation: &Expr,
    input: &Type,
    fields: &BTreeMap<String, Type>,
    domains: &ReturnDomains,
    cgs: &CGS,
    entry: &str,
    symbols: &dyn SymbolResolve,
) -> Result<Argument, String> {
    if is_row(annotation) {
        return Ok(Argument {
            field: None,
            per_row: true,
            value_type: input.clone(),
        });
    }
    if matches!(annotation, Expr::Subscript(s) if name(&s.value) == Some("list") && is_row(&s.slice))
    {
        return Ok(Argument {
            field: None,
            per_row: false,
            value_type: input.clone(),
        });
    }
    let expected = returns::resolve(
        annotation,
        input,
        &domains.types,
        &domains.catalogs,
        cgs,
        entry,
        symbols,
    )?;
    let mut columns = fields.iter();
    let Some((field, actual)) = columns.next().filter(|_| fields.len() == 1) else {
        return Err(
            "value compute requires exactly one value column; project the intended field".into(),
        );
    };
    // Prefer a value already having the annotated shape. Only a list annotation
    // that does not match that value can collect the single column across rows.
    let per_row = if compatible(actual, &expected) {
        true
    } else if let Shape::Array { element } = &expected.shape {
        if !compatible(actual, element) {
            return Err("compute input type differs from the value column".into());
        }
        false
    } else {
        return Err("compute input type differs from the value column".into());
    };
    Ok(Argument {
        field: Some(field.clone()),
        per_row,
        value_type: expected,
    })
}

fn compatible(actual: &Type, expected: &Type) -> bool {
    if let Shape::Union { variants } = &actual.shape {
        return variants.iter().all(|v| compatible(v, expected));
    }
    if let Shape::Union { variants } = &expected.shape {
        return variants.iter().any(|v| compatible(actual, v));
    }
    match (&actual.shape, &expected.shape) {
        (Shape::Temporal { kind: a, .. }, Shape::Temporal { kind: b, .. }) => {
            a == b && (!actual.nullable || expected.nullable)
        }
        (Shape::Never, _) => true,
        (Shape::Null, _) => expected.nullable || expected.shape == Shape::Null,
        (Shape::Array { element: a }, Shape::Array { element: b }) => compatible(a, b),
        (
            Shape::Record { fields: a } | Shape::ObservedRecord { fields: a, .. },
            Shape::Record { fields: b } | Shape::ObservedRecord { fields: b, .. },
        ) => b
            .iter()
            .all(|(k, b)| a.get(k).is_some_and(|a| compatible(a, b))),
        (Shape::Scalar { field_type: a }, Shape::Scalar { field_type: b }) => {
            if a == b {
                return true;
            }
            use FieldType::*;
            let string = |t: &FieldType| match t {
                String | Uuid | DigitId | Select => true,
                Boolean
                | Integer
                | Number
                | Date
                | Money
                | MultiSelect
                | EntityRef { .. }
                | Json
                | Blob
                | Array => false,
            };
            (a == &Integer && b == &Number) || (string(a) && string(b))
        }
        (
            Shape::Scalar {
                field_type: FieldType::MultiSelect,
            },
            Shape::Array { element },
        ) => compatible(&Type::scalar(FieldType::String), element),
        _ => false,
    }
}

impl Argument {
    pub fn expression(&self) -> String {
        match (&self.field, self.per_row) {
            (None, true) => "__input[0]".into(),
            (None, false) => "__input".into(),
            (Some(field), true) => format!("__input[0].{field}"),
            (Some(field), false) => format!("[row.{field} for row in __input]"),
        }
    }
    pub fn validate(
        &self,
        rows: &crate::python_pool::TypedRecords,
        cgs: &CGS,
        entry: &str,
        catalogs: &BTreeMap<String, std::sync::Arc<CGS>>,
    ) -> Result<(), String> {
        let Some(field) = &self.field else {
            return Ok(());
        };
        let values = rows
            .iter()
            .map(|row| {
                row.get(field)
                    .cloned()
                    .ok_or_else(|| format!("compute input field {field} is unobserved"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let input = if self.per_row {
            let [value] = values.as_slice() else {
                return Err("value compute invocation requires one row".into());
            };
            value.clone()
        } else {
            Value::Array(values)
        };
        self.value_type
            .validate_in(&input, cgs, entry, "compute argument", &|entry| {
                catalogs.get(entry).map(AsRef::as_ref)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn value_adaptation_keeps_boolean_numeric_union_and_array_distinctions() {
        let integer = Type::scalar(FieldType::Integer);
        let number = Type::scalar(FieldType::Number);
        let boolean = Type::scalar(FieldType::Boolean);
        let text = Type::scalar(FieldType::String);
        assert!(compatible(&integer, &number));
        assert!(!compatible(&number, &integer));
        assert!(!compatible(&boolean, &integer));
        let union = Type::join(integer.clone(), text.clone());
        assert!(compatible(&integer, &union));
        assert!(!compatible(&union, &integer));
        let array = |element| Type {
            shape: Shape::Array {
                element: Box::new(element),
            },
            domain: None,
            nullable: false,
        };
        assert!(compatible(&array(integer.clone()), &array(number)));
        assert!(!compatible(&array(text), &array(integer.clone())));
        let argument = Argument {
            field: Some("n".into()),
            per_row: true,
            value_type: integer,
        };
        let cgs = CGS::default();
        assert!(argument
            .validate(
                &vec![BTreeMap::from([("n".into(), Value::Null)])],
                &cgs,
                "types",
                &BTreeMap::new()
            )
            .is_err());
        assert!(argument
            .validate(&vec![BTreeMap::new()], &cgs, "types", &BTreeMap::new())
            .is_err());
        assert!(argument
            .validate(&vec![], &cgs, "types", &BTreeMap::new())
            .is_err());
    }
}
