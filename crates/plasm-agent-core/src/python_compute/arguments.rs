//! Boundary adaptation only: Monty owns the function body and Python semantics.
use super::*;
use plasm_core::value_contract::{ValueContract as Type, ValueShape as Shape};

#[derive(Debug, thiserror::Error)]
pub enum PythonArgumentError {
    #[error("plural compute source supplies list[Row]; use a map callback for per-row work")]
    PluralSourceRequiresMapCallback,
    #[error("singleton compute source supplies Row, not list[Row]")]
    SingletonSourceRequiresRow,
    #[error("value compute requires exactly one value column; project the intended field")]
    ValueSourceRequiresOneColumn,
    #[error("compute source input differs from its annotation")]
    AnnotationMismatch(#[source] Box<crate::python_compute::inference::InferenceError>),
    #[error(transparent)]
    Inference(#[from] crate::python_compute::inference::InferenceError),
    #[error("compute input field {field} is unobserved")]
    InputFieldUnobserved { field: String },
    #[error("value compute invocation requires one row")]
    ValueInvocationRequiresOneRow,
    #[error("compute input value violates its declared contract")]
    ValueContract(#[source] plasm_core::value_contract::ValueContractError),
}

pub(super) struct Argument {
    pub field: Option<String>,
    pub per_row: bool,
    pub value_type: Type,
    pub mapping: Option<String>,
}

pub(crate) fn is_row(expr: &Expr) -> bool {
    name(expr) == Some("Row")
        || matches!(expr, Expr::Subscript(s) if name(&s.value).is_some_and(is_entity_record_type))
}

/// Borrowed catalog and annotation environment shared by argument admission.
pub(super) struct ArgumentContext<'a> {
    pub domains: &'a ReturnDomains,
    pub cgs: &'a CGS,
    pub entry: &'a str,
    pub symbols: &'a dyn SymbolResolve,
    pub imports: &'a str,
}

pub(super) fn resolve(
    annotation: &Expr,
    input: &Type,
    fields: &BTreeMap<String, Type>,
    context: &ArgumentContext<'_>,
    mode: ComputeInputMode,
) -> Result<Argument, PythonArgumentError> {
    let &ArgumentContext {
        domains,
        cgs,
        entry,
        symbols,
        imports,
    } = context;
    if is_row(annotation) {
        if mode != ComputeInputMode::Singleton {
            return Err(PythonArgumentError::PluralSourceRequiresMapCallback);
        }
        return Ok(Argument {
            mapping: None,
            field: None,
            per_row: true,
            value_type: input.clone(),
        });
    }
    if matches!(annotation, Expr::Subscript(s) if name(&s.value) == Some("list") && is_row(&s.slice))
    {
        if mode != ComputeInputMode::Collection {
            return Err(PythonArgumentError::SingletonSourceRequiresRow);
        }
        return Ok(Argument {
            mapping: None,
            field: None,
            per_row: false,
            value_type: input.clone(),
        });
    }
    let expected = returns::prepare(annotation, input, domains, cgs, entry, symbols, imports)?;
    // Preserve a declared value column when it already satisfies the input.
    // A mapping view is an adaptation of a record, not a wrapper around an
    // existing dictionary-valued column.
    if fields.len() == 1 {
        let (field, actual) = fields.iter().next().expect("one field");
        let collection = Type {
            shape: Shape::Array {
                element: Box::new(actual.clone()),
            },
            domain: None,
            nullable: false,
        };
        let selected = if mode.per_row() { actual } else { &collection };
        if expected.check(selected).is_ok() {
            return Ok(Argument {
                mapping: None,
                field: Some(field.clone()),
                per_row: mode.per_row(),
                value_type: selected.clone(),
            });
        }
    }
    // Ask Monty whether a typed record constructor satisfies the authored
    // annotation. The body keeps the complete field contract, never Any.
    let entries = fields
        .keys()
        .map(|key| {
            let key = serde_json::to_string(key).expect("field name");
            format!("{key}: row[{key}]")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let per_row = mode.per_row();
    let value = if per_row {
        format!("{{{entries}}}")
    } else {
        format!("[{{{entries}}}]")
    };
    if expected
        .check_body(
            &format!("\n    return {value}\n"),
            &[("row", input)],
            cgs,
            &domains.catalogs,
        )
        .is_ok()
    {
        let mapping = Type {
            shape: Shape::MappingRecord {
                record: Box::new(input.clone()),
            },
            domain: None,
            nullable: false,
        };
        return Ok(Argument {
            field: None,
            per_row,
            mapping: Some({
                let keys = serde_json::to_string(&fields.keys().collect::<Vec<_>>()).expect("keys");
                let record = format!("{{key: row[key] for key in {keys} if hasattr(row, key)}}");
                if per_row {
                    format!("[{record} for row in __input][0]")
                } else {
                    format!("[{record} for row in __input]")
                }
            }),
            value_type: if per_row {
                mapping
            } else {
                Type {
                    shape: Shape::Array {
                        element: Box::new(mapping),
                    },
                    domain: None,
                    nullable: false,
                }
            },
        });
    }
    let mut columns = fields.iter();
    let Some((field, actual)) = columns.next().filter(|_| fields.len() == 1) else {
        return Err(PythonArgumentError::ValueSourceRequiresOneColumn);
    };
    // Check only the source-selected materialization mode. Monty owns Python
    // subtyping and annotation compatibility.
    let collection = Type {
        shape: Shape::Array {
            element: Box::new(actual.clone()),
        },
        domain: None,
        nullable: false,
    };
    let per_row = mode.per_row();
    expected
        .check(if per_row { actual } else { &collection })
        .map_err(|error| PythonArgumentError::AnnotationMismatch(Box::new(error)))?;
    Ok(Argument {
        mapping: None,
        field: Some(field.clone()),
        per_row,
        value_type: if per_row {
            actual.clone()
        } else {
            Type {
                shape: Shape::Array {
                    element: Box::new(actual.clone()),
                },
                domain: None,
                nullable: false,
            }
        },
    })
}

impl Argument {
    pub fn expression(&self) -> String {
        if let Some(expression) = &self.mapping {
            return expression.clone();
        }
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
    ) -> Result<(), PythonArgumentError> {
        let Some(field) = &self.field else {
            return Ok(());
        };
        let values = rows
            .iter()
            .map(|row| {
                row.get(field)
                    .cloned()
                    .ok_or_else(|| PythonArgumentError::InputFieldUnobserved {
                        field: field.clone(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let input = if self.per_row {
            let [value] = values.as_slice() else {
                return Err(PythonArgumentError::ValueInvocationRequiresOneRow);
            };
            value.clone()
        } else {
            Value::Array(values)
        };
        self.value_type
            .validate_in(&input, cgs, entry, "compute argument", &|entry| {
                catalogs.get(entry).map(AsRef::as_ref)
            })
            .map_err(PythonArgumentError::ValueContract)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn compatible(actual: &Type, expected: &Type) -> bool {
        inference::assignable(actual, expected).unwrap()
    }
    #[test]
    fn value_adaptation_keeps_boolean_numeric_union_and_array_distinctions() {
        let integer = Type::scalar(FieldType::Integer);
        let number = Type::scalar(FieldType::Number);
        let boolean = Type::scalar(FieldType::Boolean);
        let text = Type::scalar(FieldType::String);
        assert!(compatible(&integer, &number));
        assert!(!compatible(&number, &integer));
        assert!(compatible(&boolean, &integer));
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
        assert!(!compatible(&array(integer.clone()), &array(number)));
        assert!(!compatible(&array(text), &array(integer.clone())));
        let argument = Argument {
            mapping: None,
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
