//! Shared clap construction and ArgMatches extraction for `InputType::Object` capability params.

use clap::{builder::PossibleValuesParser, Arg, ArgAction, ArgMatches};
use plasm_core::{
    CapabilitySchema, CompOp, FieldType, InputFieldSchema, InputFieldWire, Predicate, Value, CGS,
};

use crate::subcommand_util::leak;

#[derive(Clone, Copy)]
pub(crate) enum FieldArgHelp {
    Invoke,
    Query,
}

/// Build one clap `Arg` for a declared object field (invoke vs query help text differs).
pub(crate) fn arg_for_input_field(field: &InputFieldSchema, help: FieldArgHelp, cgs: &CGS) -> Arg {
    let name: &'static str = leak(field.name.clone());
    let mut arg = Arg::new(name).long(name);

    match &field.wire {
        InputFieldWire::Inline(_) => {
            arg = arg
                .value_parser(clap::value_parser!(String))
                .help("structured parameter (JSON)");
        }
        InputFieldWire::Registry(_) => {
            let nv = field
                .named_value(cgs)
                .expect("input field value_ref must resolve for CLI");

            arg = match nv.field_type {
                FieldType::Number => arg.value_parser(clap::value_parser!(f64)),
                FieldType::Integer => arg.value_parser(clap::value_parser!(i64)),
                FieldType::Boolean => arg.action(ArgAction::SetTrue),
                FieldType::Select => {
                    if let Some(ref allowed) = nv.allowed_values {
                        let vals: Vec<&'static str> =
                            allowed.iter().map(|s| leak(s.clone())).collect();
                        arg.value_parser(PossibleValuesParser::new(vals))
                    } else {
                        arg
                    }
                }
                FieldType::MultiSelect | FieldType::Array => arg.action(ArgAction::Append),
                _ => arg,
            };
        }
    }

    if field.required {
        arg = arg.required(true);
    }

    match help {
        FieldArgHelp::Invoke => {
            if let Some(ref desc) = field.description {
                arg = arg.help(desc.clone());
            }
        }
        FieldArgHelp::Query => {
            if let Some(ref desc) = field.description {
                arg = arg.help(desc.clone());
            } else if matches!(&field.wire, InputFieldWire::Registry(_)) {
                let nv = field.named_value(cgs).expect("input field value_ref");
                let h = match nv.field_type {
                    FieldType::Select => {
                        if let Some(ref av) = nv.allowed_values {
                            format!("{} [{}]", field.name, av.join(", "))
                        } else {
                            field.name.clone()
                        }
                    }
                    FieldType::Boolean => format!("{} (flag)", field.name),
                    FieldType::MultiSelect | FieldType::Array => {
                        format!("{} (repeatable)", field.name)
                    }
                    _ => field.name.clone(),
                };
                arg = arg.help(h);
            }
        }
    }

    arg
}

pub(crate) fn build_field_args(cap: &CapabilitySchema, help: FieldArgHelp, cgs: &CGS) -> Vec<Arg> {
    let fields: Vec<&plasm_core::InputFieldSchema> = match help {
        FieldArgHelp::Query => cap.query_surface_fields().collect(),
        FieldArgHelp::Invoke => cap.invocation_object_fields().collect(),
    };
    if fields.is_empty() {
        return vec![];
    }
    fields
        .into_iter()
        .map(|f| arg_for_input_field(f, help, cgs))
        .collect()
}

/// Extract a single field's value from clap matches (invoke / generic object assembly).
pub(crate) fn field_value_for_invoke(
    matches: &ArgMatches,
    field: &InputFieldSchema,
    cgs: &CGS,
) -> Option<Value> {
    match &field.wire {
        InputFieldWire::Inline(_) => {
            let s = matches.get_one::<String>(&field.name)?;
            serde_json::from_str::<Value>(s).ok()
        }
        InputFieldWire::Registry(_) => {
            let nv = field.named_value(cgs).ok()?;
            match nv.field_type {
                FieldType::Number => matches
                    .get_one::<f64>(&field.name)
                    .copied()
                    .map(Value::Float),
                FieldType::Integer => matches
                    .get_one::<i64>(&field.name)
                    .copied()
                    .map(Value::Integer),
                FieldType::Boolean => {
                    if matches.get_flag(&field.name) {
                        Some(Value::Bool(true))
                    } else {
                        None
                    }
                }
                FieldType::MultiSelect | FieldType::Array => matches
                    .get_many::<String>(&field.name)
                    .map(|vals| Value::Array(vals.map(|v| Value::String(v.clone())).collect())),
                _ => matches
                    .get_one::<String>(&field.name)
                    .map(|s| Value::String(s.clone())),
            }
        }
    }
}

/// Append query predicates for one field (equality / `In` for multi-select).
pub(crate) fn extend_query_predicates_for_field(
    matches: &ArgMatches,
    field: &InputFieldSchema,
    out: &mut Vec<Predicate>,
    cgs: &CGS,
) {
    match &field.wire {
        InputFieldWire::Inline(_) => {
            if let Some(val) = matches.get_one::<String>(&field.name) {
                out.push(Predicate::eq(&field.name, val.as_str()));
            }
            return;
        }
        InputFieldWire::Registry(_) => {}
    }
    let Ok(nv) = field.named_value(cgs) else {
        return;
    };
    match nv.field_type {
        FieldType::Number => {
            if let Some(&val) = matches.get_one::<f64>(&field.name) {
                out.push(Predicate::comparison(
                    &field.name,
                    CompOp::Eq,
                    Value::Float(val),
                ));
            }
        }
        FieldType::Integer => {
            if let Some(&val) = matches.get_one::<i64>(&field.name) {
                out.push(Predicate::comparison(
                    &field.name,
                    CompOp::Eq,
                    Value::Integer(val),
                ));
            }
        }
        FieldType::Boolean => {
            if matches.get_flag(&field.name) {
                out.push(Predicate::eq(&field.name, true));
            }
        }
        FieldType::MultiSelect | FieldType::Array => {
            if let Some(vals) = matches.get_many::<String>(&field.name) {
                let arr: Vec<Value> = vals.map(|v| Value::String(v.clone())).collect();
                if !arr.is_empty() {
                    out.push(Predicate::comparison(
                        &field.name,
                        CompOp::In,
                        Value::Array(arr),
                    ));
                }
            }
        }
        _ => {
            if let Some(val) = matches.get_one::<String>(&field.name) {
                out.push(Predicate::eq(&field.name, val.as_str()));
            }
        }
    }
}
