mod admission;
pub(crate) use admission::render_analysis_diagnostics;
#[cfg(test)]
mod analysis_tests;
pub(crate) mod arguments;
mod multiple;
pub(crate) use arguments::is_row as is_row_annotation;
pub(crate) mod inference;
pub use arguments::PythonArgumentError;
pub use inference::{InferenceError, InferenceGraphError};
pub mod predicate_facts;
mod returns;
mod upstream;
pub(crate) use upstream::admit_bundle;
pub(crate) mod schema;
pub(crate) use schema::source_field_kind;
use schema::validate_source_owner;
// Restricted pure Python compute boundary for the experimental DAG slice.
use plasm_core::collection_codec::{
    CollectionCodec, CollectionFault, Demand, RecordedCollection, RecordingCodec,
};
use plasm_core::plasm_monad::SyntheticResultSchema;
use plasm_core::symbol_tuning::{EntityBinding, SymbolResolve};
use plasm_core::{FieldType, CGS};
use plasm_core::{Value, ValueRow};
use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;
use std::collections::BTreeMap;

use crate::program_rejection::{PythonComputeError, PythonComputeRejection};

/// Infer a pure helper's materialization port from the already typed DAG
/// dependency. `Row` denotes the complete incoming record contract; Monty
/// checks the helper body against that contract before the plan is admitted.
pub(crate) fn inferred_helper_input_annotation(
    row: &plasm_core::value_contract::ValueContract,
    scalar_cell: bool,
    collection: bool,
) -> Result<String, crate::program_rejection::PythonProgramError> {
    let value = if scalar_cell {
        row.field("value").map_err(|_| {
            crate::program_rejection::PythonProgramError::HelperScalarCellValueMissing
        })?
    } else {
        row.clone()
    };
    fn annotation(
        value: &plasm_core::value_contract::ValueContract,
    ) -> Result<String, crate::program_rejection::PythonProgramError> {
        use plasm_core::value_contract::ValueShape;
        let mut required = value.clone();
        required.nullable = false;
        let base = match &required.shape {
            ValueShape::Record { .. } | ValueShape::ObservedRecord { .. } => "Row".into(),
            ValueShape::Array { element } => format!("list[{}]", annotation(element)?),
            ValueShape::Set { element } => format!("set[{}]", annotation(element)?),
            ValueShape::Dictionary { key, value } => {
                format!("dict[{}, {}]", annotation(key)?, annotation(value)?)
            }
            ValueShape::Scalar { field_type } => match field_type {
                FieldType::Boolean => "bool",
                FieldType::Integer => "int",
                FieldType::Number => "float",
                FieldType::MultiSelect => "list[str]",
                FieldType::Money => "PlasmMoney",
                FieldType::Date | FieldType::Array => {
                    return Err(crate::program_rejection::PythonProgramError::HelperInputRequiresTemporalOrArrayContract);
                }
                FieldType::EntityRef { .. } | FieldType::Json | FieldType::Blob => {
                    return Err(crate::program_rejection::PythonProgramError::HelperInputRequiresMaterializedValueContract);
                }
                _ => "str",
            }
            .into(),
            ValueShape::Temporal { kind, .. } => format!("datetime.{}", kind.python_name()),
            ValueShape::Union { variants } => variants
                .iter()
                .map(annotation)
                .collect::<Result<Vec<_>, _>>()?
                .join(" | "),
            ValueShape::Null => "None".into(),
            ValueShape::Never => "Never".into(),
            ValueShape::MappingRecord { .. } => {
                return Err(crate::program_rejection::PythonProgramError::HelperInputRequiresMappingAnnotation);
            }
        };
        Ok(
            if value.nullable && !matches!(value.shape, ValueShape::Null) {
                format!("{base} | None")
            } else {
                base
            },
        )
    }
    let value = annotation(&value)?;
    Ok(if collection {
        format!("list[{value}]")
    } else {
        value
    })
}

/// A callback receives one materialized row. The checker judges any declared
/// Python parameter type; the host supplies catalog references and provenance.
pub(crate) fn check_row_parameter(
    session: &crate::execute_session::ExecuteSession,
    annotation: &Expr,
    actual: &plasm_core::value_contract::ValueContract,
    imports: &str,
) -> Result<(), PythonComputeRejection> {
    check_callback_return(session, annotation, actual, actual, imports)
}

pub(crate) fn check_callback_return(
    session: &crate::execute_session::ExecuteSession,
    annotation: &Expr,
    input: &plasm_core::value_contract::ValueContract,
    actual: &plasm_core::value_contract::ValueContract,
    imports: &str,
) -> Result<(), PythonComputeRejection> {
    let context = session
        .contexts_by_entry
        .get(&session.entry_id)
        .ok_or(PythonComputeError::CallbackContextMissing)?;
    let symbols = crate::plasm_plan_run::symbol_map_for_plasm_surface_parse(session, None);
    let domains = return_domains(session)?;
    let annotation = returns::prepare(
        annotation,
        input,
        &domains,
        &context.cgs,
        &session.entry_id,
        symbols.as_ref(),
        imports,
    )
    .map_err(PythonComputeRejection::from)?;
    annotation
        .check(actual)
        .map_err(PythonComputeRejection::from)
}

pub(crate) fn check_callback_closed_return(
    session: &crate::execute_session::ExecuteSession,
    annotation: &Expr,
    input: &plasm_core::value_contract::ValueContract,
    expression: &Expr,
    imports: &str,
) -> Result<(), PythonComputeRejection> {
    let context = session
        .contexts_by_entry
        .get(&session.entry_id)
        .ok_or(PythonComputeError::CallbackContextMissing)?;
    let symbols = crate::plasm_plan_run::symbol_map_for_plasm_surface_parse(session, None);
    let domains = return_domains(session)?;
    let annotation = returns::prepare(
        annotation,
        input,
        &domains,
        &context.cgs,
        &session.entry_id,
        symbols.as_ref(),
        imports,
    )
    .map_err(PythonComputeRejection::from)?;
    annotation
        .check_body(
            &format!("\n    return ({})\n", monty::expression_source(expression)),
            &[],
            &context.cgs,
            &domains.catalogs,
        )
        .map_err(PythonComputeRejection::from)
}

/// A DAG record literal is a Python dictionary expression before its fields are
/// published as named columns. Preserve that contextual typing for annotations.
pub(crate) fn check_callback_record_return(
    session: &crate::execute_session::ExecuteSession,
    annotation: &Expr,
    input: &plasm_core::value_contract::ValueContract,
    actual: &plasm_core::value_contract::ValueContract,
    imports: &str,
) -> Result<(), PythonComputeRejection> {
    let context = session
        .contexts_by_entry
        .get(&session.entry_id)
        .ok_or(PythonComputeError::CallbackContextMissing)?;
    let symbols = crate::plasm_plan_run::symbol_map_for_plasm_surface_parse(session, None);
    let domains = return_domains(session)?;
    let annotation = returns::prepare(
        annotation,
        input,
        &domains,
        &context.cgs,
        &session.entry_id,
        symbols.as_ref(),
        imports,
    )
    .map_err(PythonComputeRejection::from)?;
    let plasm_core::value_contract::ValueShape::Record { fields } = &actual.shape else {
        return Err(PythonComputeError::CallbackRecordContractMissing.into());
    };
    let entries = fields
        .keys()
        .map(|name| {
            let key = serde_json::to_string(name).map_err(|source| {
                PythonComputeError::ValueContractLiteralEncoding(source.into())
            })?;
            Ok(format!("{key}: result[{key}]"))
        })
        .collect::<Result<Vec<_>, PythonComputeError>>()?
        .join(", ");
    annotation
        .check_body(
            &format!("\n    return {{{entries}}}\n"),
            &[("result", actual)],
            &context.cgs,
            &domains.catalogs,
        )
        .map_err(PythonComputeRejection::from)
}

pub(crate) const LANGUAGE_PROFILE: &str =
    "monty-e007685fbb06494c13b9a7b3fede9f8e6a54a2be-typed-v11-money-v2-branches-v2";

pub(crate) const CONTRACT_VERSION: u32 = 12;

pub(crate) const MAX_INPUT_ROWS: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum ComputeInputBudgetError {
    #[error("compute input row budget exceeded")]
    RowCountExceeded,
    #[error(transparent)]
    Value(#[from] plasm_core::ValueBudgetError),
}

#[derive(Debug, Clone)]
pub struct ValueContract {
    pub owner: EntityBinding,
    pub fields: BTreeMap<String, FieldContract>,
    description: String,
    cgs: std::sync::Arc<CGS>,
}

#[derive(Debug, Clone)]
pub struct FieldContract {
    pub value_type: plasm_core::value_contract::ValueContract,
    pub required: bool,
    pub domain: Option<plasm_core::value_domain::ValueDomain>,
    description: String,
}

impl ValueContract {
    pub fn from_cgs(
        cgs: &CGS,
        entry: &str,
        symbols: &dyn SymbolResolve,
        token: &str,
    ) -> Result<Self, PythonComputeError> {
        let owner = symbols
            .resolve_session_entity(token)
            .map_err(PythonComputeError::ValueContractSymbol)?;
        if owner.entry_id.as_str() != entry {
            return Err(PythonComputeError::ValueContractOwnerMismatch);
        }
        let entity = cgs.get_entity(owner.entity.as_str()).ok_or_else(|| {
            PythonComputeError::ValueContractEntityMissing {
                entity: owner.entity.to_string(),
            }
        })?;
        let mut fields = BTreeMap::new();
        for field in entity.fields.values() {
            let value = field.named_value(cgs).map_err(|_| {
                PythonComputeError::ValueContractFieldMissing {
                    field: field.name.to_string(),
                }
            })?;
            let mut value_type = plasm_core::value_contract::ValueContract::from_domain(
                cgs,
                entry,
                field.kind.registry_key(),
            )
            .map_err(PythonComputeError::from)?;
            value_type.nullable = !field.required;
            fields.insert(
                field.name.to_string(),
                FieldContract {
                    value_type,
                    required: field.required,
                    domain: Some(value.domain.clone()),
                    description: value.description.clone(),
                },
            );
        }
        // Embedded relation observations are values, not traversals. Reading these
        // references neither hydrates targets nor establishes relation completeness.
        for relation in entity.relations.values() {
            fields
                .entry(relation.name.to_string())
                .or_insert_with(|| FieldContract {
                    value_type: observed_relation_type(relation, entry),
                    required: true,
                    domain: None,
                    description: format!(
                        "Observed relation references only; no traversal or completeness claim. {}",
                        relation.description
                    ),
                });
        }
        Ok(Self {
            cgs: std::sync::Arc::new(cgs.clone()),
            owner,
            fields,
            description: entity.description.clone(),
        })
    }

    /// Render the concrete materialized view used by this probe from the same descriptor.
    /// Value symbols remain representation aliases; catalog/entity ownership is checked separately.
    pub fn declaration(
        &self,
        symbols: &plasm_core::SymbolMap,
    ) -> Result<String, PythonComputeError> {
        let token =
            symbols.entity_sym_for(self.owner.entry_id.as_str(), self.owner.entity.as_str());
        let mut aliases = BTreeMap::new();
        let mut members = String::new();
        for (field_name, value) in &self.fields {
            let alias = symbols.value_sym_for_wire(
                self.owner.entry_id.as_str(),
                self.owner.entity.as_str(),
                field_name,
            );
            let representation =
                if let Some(tokens) = value.domain.as_ref().and_then(|d| d.enum_tokens()) {
                    let tokens = tokens
                        .iter()
                        .map(serde_json::to_string)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|source| {
                            PythonComputeError::ValueContractLiteralEncoding(source.into())
                        })?;
                    let literal = format!("Literal[{}]", tokens.join(", "));
                    if matches!(
                        value.value_type.shape,
                        plasm_core::value_contract::ValueShape::Scalar {
                            field_type: FieldType::MultiSelect
                        }
                    ) {
                        format!("list[{literal}]")
                    } else {
                        literal
                    }
                } else {
                    let ty = value.value_type.python_type();
                    if value.required {
                        ty
                    } else {
                        ty.trim_end_matches(" | None").to_owned()
                    }
                };
            let member_type = match alias {
                Some(alias) => {
                    aliases.insert(alias.clone(), representation);
                    alias
                }
                None if value.domain.is_none() => representation,
                None => {
                    return Err(PythonComputeError::ValueContractSymbolMissing {
                        field: field_name.clone(),
                    })
                }
            };
            for line in value.description.lines() {
                members.push_str(&format!("    # {line}\n"));
            }
            members.push_str(&format!(
                "    {field_name}: {member_type}{}\n",
                if value.required { "" } else { " | None" }
            ));
        }
        let mut out = String::from("from typing import Literal\n");
        for (alias, representation) in aliases {
            out.push_str(&format!("type {alias} = {representation}\n"));
        }
        for line in self.description.lines() {
            out.push_str(&format!("# {line}\n"));
        }
        out.push_str(&format!("# Materialized Value[{token}]; no domain methods or relation traversal.\nclass {token}Value:\n{members}"));
        Ok(out)
    }

    pub fn materialize(
        &self,
        owner: &EntityBinding,
        membership: &RecordedCollection<plasm_core::Ref>,
        rows: &[ValueRow],
    ) -> Result<crate::python_pool::TypedRecords, PythonComputeError> {
        if owner != &self.owner {
            return Err(PythonComputeError::ComputeInputOwnerMismatch);
        }
        validate_collection_input(membership, rows.len())
            .map_err(PythonComputeError::ComputeInputMembership)?;
        validate_input_budget(rows).map_err(|error| match error {
            ComputeInputBudgetError::RowCountExceeded => {
                PythonComputeError::ComputeInputRowBudgetExceeded
            }
            ComputeInputBudgetError::Value(error) => {
                PythonComputeError::ComputeInputValueBudget(error)
            }
        })?;
        let mut values = Vec::with_capacity(rows.len());
        for row in rows {
            let mut attrs = BTreeMap::new();
            for (field, contract) in &self.fields {
                let Some(value) = row.get(field) else {
                    continue;
                };
                if value.is_null() && !contract.required {
                    attrs.insert(field.clone(), Value::Null);
                    continue;
                }
                let value = contract
                    .value_type
                    .observed_value(value, &self.cgs, self.owner.entry_id.as_str())
                    .map_err(PythonComputeError::ComputeInputValueContract)?;
                contract
                    .value_type
                    .validate(&value, &self.cgs, self.owner.entry_id.as_str(), field)
                    .map_err(PythonComputeError::ComputeInputValueContract)?;
                attrs.insert(field.clone(), value);
            }
            values.push(attrs);
        }
        Ok(values)
    }
}

pub(crate) fn observed_relation_type(
    relation: &plasm_core::RelationSchema,
    entry: &str,
) -> plasm_core::value_contract::ValueContract {
    use plasm_core::value_contract::{ValueContract as T, ValueShape};
    let mut target = T::scalar(FieldType::EntityRef {
        entry_id: entry.into(),
        target: relation.target_resource.clone(),
    });
    match relation.cardinality {
        plasm_core::Cardinality::Many => T {
            shape: ValueShape::Array {
                element: Box::new(target),
            },
            domain: None,
            nullable: false,
        },
        plasm_core::Cardinality::One => {
            target.nullable = true;
            target
        }
    }
}

/// Keep body line numbers (including multiline signatures) without evaluating
/// caller-controlled annotations, defaults or decorators.
pub(crate) fn definition_body(
    source: &str,
    def: &ruff_python_ast::StmtFunctionDef,
) -> Result<String, PythonComputeRejection> {
    let start = def
        .body
        .first()
        .ok_or(PythonComputeError::ComputeBodyMissing)?
        .start()
        .to_usize();
    let lines = source[def.name.start().to_usize()..start]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    if lines == 0 {
        Ok(format!(" {}", &source[start..def.end().to_usize()]))
    } else {
        let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
        Ok(format!(
            "{}{}",
            "\n".repeat(lines),
            &source[line_start..def.end().to_usize()]
        ))
    }
}

fn diagnostic_imports(
    source: &str,
    def: &ruff_python_ast::StmtFunctionDef,
    imports: &str,
) -> String {
    let line = source[..def.name.start().to_usize()]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    format!(
        "{imports}{}",
        "\n".repeat(line.saturating_sub(imports.lines().count() + 1))
    )
}

pub struct PreparedCompute {
    independent_inputs: bool,
    optional_fields: std::collections::BTreeSet<String>,
    pub contract: Option<ValueContract>,
    context: std::sync::Arc<CGS>,
    entry: String,
    argument: arguments::Argument,
    executable: String,
    definition: String,
    stubs: String,
    row_fields: Option<BTreeMap<String, plasm_core::value_contract::ValueContract>>,
    pub(crate) per_row: bool,
    pub(crate) output: plasm_core::value_contract::ValueContract,
    catalogs: BTreeMap<String, std::sync::Arc<CGS>>,
}

/// Materialization is chosen by the source DAG, never by Python annotation syntax.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComputeInputMode {
    Singleton,
    Collection,
}

impl ComputeInputMode {
    fn per_row(self) -> bool {
        matches!(self, Self::Singleton)
    }
}

impl PreparedCompute {
    pub fn prepare(
        source: &str,
        cgs: &CGS,
        entry: &str,
        symbols: &dyn SymbolResolve,
        mode: ComputeInputMode,
    ) -> Result<Self, PythonComputeRejection> {
        Self::prepare_input(source, cgs, entry, symbols, None, mode)
    }

    pub(crate) fn prepare_input(
        source: &str,
        cgs: &CGS,
        entry: &str,
        symbols: &dyn SymbolResolve,
        rows: Option<(&SyntheticResultSchema, &str)>,
        mode: ComputeInputMode,
    ) -> Result<Self, PythonComputeRejection> {
        Self::prepare_typed(
            source,
            cgs,
            entry,
            symbols,
            rows,
            &ReturnDomains::default(),
            mode,
        )
    }

    pub(crate) fn prepare_typed(
        source: &str,
        cgs: &CGS,
        entry: &str,
        symbols: &dyn SymbolResolve,
        rows: Option<(&SyntheticResultSchema, &str)>,
        domains: &ReturnDomains,
        mode: ComputeInputMode,
    ) -> Result<Self, PythonComputeRejection> {
        if source.len() > 4096 {
            return Err(PythonComputeError::SourceBudgetExceeded.into());
        }
        let ast =
            ruff_python_parser::parse_module(source).map_err(PythonComputeRejection::Parse)?;
        let (imports, suite) = crate::python_datetime::Imports::split(source, ast.suite())
            .map_err(PythonComputeRejection::from)?;
        let [Stmt::FunctionDef(def)] = suite else {
            return Err(PythonComputeError::FunctionCount.into());
        };
        if def.is_async
            || def.type_params.is_some()
            || def.decorator_list.len() != 1
            || name(&def.decorator_list[0].expression) != Some("compute")
        {
            return Err(PythonComputeError::FunctionShape.into());
        }
        if def.parameters.args.len() > 1 {
            if mode != ComputeInputMode::Singleton {
                return Err(PythonComputeError::DependencyPacketNotSingleton.into());
            }
            return Self::prepare_multiple(
                source,
                def,
                rows,
                &multiple::MultipleContext {
                    imports: &imports,
                    cgs,
                    entry,
                    symbols,
                    domains,
                },
            );
        }
        let p = &def.parameters;
        if !p.posonlyargs.is_empty()
            || !p.kwonlyargs.is_empty()
            || p.vararg.is_some()
            || p.kwarg.is_some()
            || p.args.len() != 1
            || p.args[0].default.is_some()
        {
            return Err(PythonComputeError::InputParameterShape.into());
        }
        let param = &p.args[0].parameter;
        let ann = param
            .annotation
            .as_deref()
            .ok_or(PythonComputeError::ComputeInputAnnotationMissing)?;
        let value = match ann {
            Expr::Subscript(s) if name(&s.value) == Some("list") => &*s.slice,
            _ => ann,
        };
        let row_annotation = arguments::is_row(value);
        let token = if let Some((_, token)) = rows {
            if row_annotation && name(value) != Some("Row") {
                let annotated = name(entity_record_argument(value)?)
                    .ok_or(PythonComputeError::ExpectedEntitySymbol)?;
                if annotated != token {
                    return Err(PythonComputeError::AnnotationSourceMismatch.into());
                }
            }
            (!token.is_empty()).then_some(token)
        } else {
            Some(
                name(entity_record_argument(value)?)
                    .ok_or(PythonComputeError::ExpectedEntitySymbol)?,
            )
        };
        let mut contract = token
            .map(|token| ValueContract::from_cgs(cgs, entry, symbols, token))
            .transpose()?;
        if let Some((schema, _)) = rows {
            for field in &schema.fields {
                if field
                    .value_type
                    .as_ref()
                    .is_some_and(|t| t.summary() != field.value_kind)
                {
                    return Err(PythonComputeError::SyntheticContractMismatch.into());
                }
            }
        }
        let fields = if let Some((schema, _)) = rows {
            schema
                .fields
                .iter()
                .map(|f| {
                    let ty = f.value_type.clone().ok_or_else(|| {
                        PythonComputeError::RecursiveInputContractMissing {
                            field: f.name.to_string(),
                        }
                    })?;
                    Ok::<_, PythonComputeError>((f.name.to_string(), ty))
                })
                .collect::<Result<BTreeMap<_, _>, _>>()?
        } else {
            contract
                .as_ref()
                .ok_or(PythonComputeError::NominalInputEntityMissing)?
                .fields
                .iter()
                .filter(|(_, f)| f.domain.is_some())
                .map(|(k, f)| (k.clone(), f.value_type.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        // Without an explicit source schema, only catalog fields are inputs.
        // Relation observations require an explicit projected contract.
        if rows.is_none() {
            contract
                .as_mut()
                .ok_or(PythonComputeError::NominalInputEntityMissing)?
                .fields
                .retain(|_, field| field.domain.is_some());
        }
        let input = plasm_core::value_contract::ValueContract::record(
            fields.clone(),
            rows.map(|(s, _)| s.optional_fields.clone())
                .unwrap_or_default(),
        );
        let argument = arguments::resolve(
            ann,
            &input,
            &fields,
            &arguments::ArgumentContext {
                domains,
                cgs,
                entry,
                symbols,
                imports: &imports.source,
            },
            mode,
        )
        .map_err(PythonComputeRejection::from)?;
        let per_row = argument.per_row;
        let body_imports = diagnostic_imports(source, def, &imports.source);
        let annotation = def
            .returns
            .as_deref()
            .map(|a| returns::prepare(a, &input, domains, cgs, entry, symbols, &body_imports))
            .transpose()
            .map_err(PythonComputeRejection::from)?;
        let declared_output = annotation
            .as_ref()
            .map(|a| a.contract())
            .transpose()
            .map_err(PythonComputeRejection::from)?
            .flatten();
        let input_type = if argument.field.is_some() || argument.mapping.is_some() {
            argument.value_type.clone()
        } else if per_row {
            input.clone()
        } else {
            plasm_core::value_contract::ValueContract {
                shape: plasm_core::value_contract::ValueShape::Array {
                    element: Box::new(input.clone()),
                },
                domain: None,
                nullable: false,
            }
        };
        if let Some(annotation) = &annotation {
            annotation
                .check_body(
                    &definition_body(source, def)?,
                    &[(param.name.as_str(), &input_type)],
                    cgs,
                    &domains.catalogs,
                )
                .map_err(PythonComputeRejection::from)?;
        }
        let inferred = if declared_output.is_none() {
            Some(
                inference::infer_body(
                    &definition_body(source, def)?,
                    &[(param.name.as_str(), &input_type)],
                    &body_imports,
                )
                .map_err(PythonComputeRejection::from)?,
            )
        } else {
            None
        };
        let output = match (declared_output, inferred) {
            (Some(output), _) => output,
            (None, Some(output)) => {
                if let Some(annotation) = &annotation {
                    annotation
                        .check(&output)
                        .map_err(PythonComputeRejection::from)?;
                }
                output
            }
            _ => return Err(PythonComputeError::OutputContractMissing.into()),
        };
        let mut stubs = upstream::stubs_in(&fields, cgs, &domains.catalogs)?;
        upstream::domain_aliases(&domains.types, cgs, &domains.catalogs, &mut stubs)?;
        let output_type = upstream::output_type(&output, cgs, &domains.catalogs, &mut stubs)?;
        let body = definition_body(source, def)?;
        let argument_type = if argument.field.is_some() || argument.mapping.is_some() {
            upstream::input_type(&argument.value_type, cgs, &domains.catalogs, &mut stubs)?
        } else if per_row {
            "PlasmInput".into()
        } else {
            "list[PlasmInput]".into()
        };
        let padding = "\n".repeat(
            source[..def.name.start().to_usize()]
                .bytes()
                .filter(|b| *b == b'\n')
                .count()
                .saturating_sub(imports.source.lines().count() + 1),
        );
        let definition = format!(
            "import datetime as PlasmDatetime\n{}{padding}def {}({}: {}) -> {}:{}\n",
            imports.source, def.name, param.name, argument_type, output_type, body
        );
        let executable = format!("{}\n{}({})", definition, def.name, argument.expression());
        Ok(Self {
            independent_inputs: false,
            optional_fields: rows
                .map(|(schema, _)| schema.optional_fields.clone())
                .unwrap_or_default(),
            contract,
            context: std::sync::Arc::new(cgs.clone()),
            entry: entry.into(),
            argument,
            executable,
            definition,
            stubs,
            output,
            catalogs: domains.catalogs.clone(),
            row_fields: rows.map(|_| fields),
            per_row,
        })
    }

    pub(crate) fn context_entry(&self) -> &str {
        &self.entry
    }

    pub fn admit(&self) -> Result<(), crate::compilation_error::CompilationError> {
        admission::check_definition(&self.definition, &self.stubs)
    }

    pub async fn run(
        &self,
        pool: &crate::python_pool::PythonPool,
        owner: &EntityBinding,
        membership: &RecordedCollection<plasm_core::Ref>,
        rows: &[ValueRow],
    ) -> Result<Value, plasm_runtime::ExecutionFailure> {
        if let Some(contract) = &self.contract {
            if self.row_fields.is_none() && owner != &contract.owner {
                return Err(plasm_runtime::ExecutionFailure::new(
                    plasm_runtime::FailureCause::Program,
                    "compute_input_owner_mismatch",
                    "compute input catalog/entity ownership differs from its admitted contract",
                ));
            }
        }
        validate_collection_input(membership, rows.len())?;
        admission::check_definitions(vec![(self.definition.clone(), self.stubs.clone())]).await?;
        let input = if let Some(fields) = &self.row_fields {
            schema::materialize_rows_in(
                fields,
                &self.optional_fields,
                rows,
                &self.context,
                self.entry.as_str(),
                &self.catalogs,
            )
            .map_err(|diagnostic| {
                plasm_runtime::ExecutionFailure::new(
                    plasm_runtime::FailureCause::Program,
                    "compute_input_schema_invalid",
                    diagnostic.to_string(),
                )
            })?
        } else {
            self.contract
                .as_ref()
                .ok_or_else(|| {
                    plasm_runtime::ExecutionFailure::new(
                        plasm_runtime::FailureCause::Program,
                        "compute_input_contract_missing",
                        "nominal compute input contract is absent",
                    )
                })?
                .materialize(owner, membership, rows)
                .map_err(|error| {
                    let (cause, code) = match &error {
                        PythonComputeError::ComputeInputValueContract(_) => (
                            plasm_runtime::FailureCause::Runtime,
                            "compute_input_value_contract_invalid",
                        ),
                        _ => (
                            plasm_runtime::FailureCause::Program,
                            "compute_input_contract_invalid",
                        ),
                    };
                    plasm_runtime::ExecutionFailure::new(cause, code, error.to_string())
                })?
        };
        if self.independent_inputs {
            input
                .iter()
                .flat_map(|row| row.values())
                .try_fold(0usize, |count, value| {
                    let rows = match value {
                        Value::Array(rows) => rows.len(),
                        _ => 1,
                    };
                    count
                        .checked_add(rows)
                        .filter(|count| *count <= MAX_INPUT_ROWS)
                        .ok_or_else(|| {
                            plasm_runtime::ExecutionFailure::new(
                                plasm_runtime::FailureCause::Program,
                                "compute_input_row_budget_exceeded",
                                "compute input row budget exceeded",
                            )
                        })
                })?;
        }
        let types = match &self.row_fields {
            Some(fields) => fields.clone(),
            None => self
                .contract
                .as_ref()
                .ok_or_else(|| {
                    plasm_runtime::ExecutionFailure::new(
                        plasm_runtime::FailureCause::Program,
                        "compute_input_contract_missing",
                        "nominal compute input contract is absent",
                    )
                })?
                .fields
                .iter()
                .map(|(k, f)| (k.clone(), f.value_type.clone()))
                .collect(),
        };
        self.argument
            .validate(&input, &self.context, &self.entry, &self.catalogs)
            .map_err(|error| {
                plasm_runtime::ExecutionFailure::new(
                    plasm_runtime::FailureCause::Program,
                    "compute_argument_invalid",
                    error.to_string(),
                )
            })?;
        let value = pool
            .compute_value(self.executable.clone(), input, &types, |value| {
                let value = self.encode_domain_output(value, &self.output)?;
                self.output.validate_in(
                    &value,
                    &self.context,
                    self.entry.as_str(),
                    "compute return",
                    &|entry| self.catalogs.get(entry).map(AsRef::as_ref),
                )?;
                Ok(value)
            })
            .await?;
        Ok(value)
    }

    /// Named catalog values retain their declared external representation; plain
    /// Python temporal values keep full components for subsequent computation.
    fn encode_domain_output(
        &self,
        value: Value,
        contract: &plasm_core::value_contract::ValueContract,
    ) -> Result<Value, crate::python_pool::PythonReturnValueError> {
        use plasm_core::value_contract::ValueShape;
        if value.is_null() {
            return Ok(value);
        }
        match &contract.shape {
            ValueShape::Scalar {
                field_type: FieldType::Money,
            } => {
                let mut money = crate::python_money::decode(value)?;
                if let Some(domain) = &contract.domain {
                    let cgs = if domain.entry_id == self.entry {
                        self.context.as_ref()
                    } else {
                        self.catalogs
                            .get(&domain.entry_id)
                            .ok_or_else(|| {
                                crate::python_pool::PythonReturnValueError::MoneyCatalogMissing {
                                    entry_id: domain.entry_id.clone(),
                                }
                            })?
                            .as_ref()
                    };
                    if let Some(plasm_core::ValueWireFormat::Money(format)) = cgs
                        .values
                        .get(domain.value_ref.as_str())
                        .and_then(|v| v.value_format)
                    {
                        money = money.with_format(format);
                    }
                }
                Ok(Value::Money(money))
            }
            ValueShape::Temporal { kind, wire } if contract.domain.is_some() => {
                let wire =
                    wire.unwrap_or(if *kind == plasm_core::temporal_value::TemporalKind::Date {
                        plasm_core::TemporalWireFormat::Iso8601Date
                    } else {
                        plasm_core::TemporalWireFormat::Rfc3339
                    });
                Ok(plasm_core::temporal_value::encode(&value, wire)?)
            }
            ValueShape::MappingRecord { record } => self.encode_domain_output(value, record),
            ValueShape::Array { element } | ValueShape::Set { element } => value
                .as_array()
                .ok_or(crate::python_pool::PythonReturnValueError::ExpectedArray)?
                .iter()
                .map(|v| self.encode_domain_output(v.clone(), element))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            ValueShape::Dictionary { value: element, .. } => value
                .as_object()
                .ok_or(crate::python_pool::PythonReturnValueError::ExpectedDictionary)?
                .iter()
                .map(|(k, v)| {
                    self.encode_domain_output(v.clone(), element)
                        .map(|v| (k.clone(), v))
                })
                .collect::<Result<indexmap::IndexMap<_, _>, _>>()
                .map(Value::Object),
            ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } => value
                .as_object()
                .ok_or(crate::python_pool::PythonReturnValueError::ExpectedRecord)?
                .iter()
                .map(|(k, v)| {
                    Ok((
                        k.clone(),
                        self.encode_domain_output(
                            v.clone(),
                            fields.get(k).ok_or_else(|| {
                                crate::python_pool::PythonReturnValueError::UnknownRecordField {
                                    field: k.clone(),
                                }
                            })?,
                        )?,
                    ))
                })
                .collect::<Result<indexmap::IndexMap<_, _>, _>>()
                .map(Value::Object),
            ValueShape::Union { variants } => {
                for variant in variants {
                    if let Ok(candidate) = self.encode_domain_output(value.clone(), variant) {
                        if variant
                            .validate_in(
                                &candidate,
                                &self.context,
                                &self.entry,
                                "return",
                                &|entry| self.catalogs.get(entry).map(AsRef::as_ref),
                            )
                            .is_ok()
                        {
                            return Ok(candidate);
                        }
                    }
                }
                Err(crate::python_pool::PythonReturnValueError::UnmatchedUnion)
            }
            _ => Ok(value),
        }
    }
}

fn name(expr: &Expr) -> Option<&str> {
    if let Expr::Name(n) = expr {
        Some(n.id.as_str())
    } else {
        None
    }
}
pub(crate) fn is_entity_record_type(head: &str) -> bool {
    matches!(head, "Value" | "Row")
}

fn entity_record_argument(
    expr: &Expr,
) -> Result<&Expr, crate::program_rejection::PythonComputeError> {
    match expr {
        Expr::Subscript(s) if name(&s.value).is_some_and(is_entity_record_type) => Ok(&s.slice),
        _ => Err(crate::program_rejection::PythonComputeError::ExpectedEntityRecordAnnotation),
    }
}

/// Recheck a serialized compute against the owning catalog and current session symbols.
/// This compiles code but does not execute it or manufacture sample values.
pub(crate) fn check_op(
    es: &crate::execute_session::ExecuteSession,
    op: &plasm_core::plasm_monad::ComputeOp,
) -> Result<PreparedCompute, PythonComputeRejection> {
    let plasm_core::plasm_monad::ComputeOp::Python {
        source,
        entry_id,
        entity,
        catalog_hash,
        contract_version,
        language_profile,
        input_schema,
        output_type,
        per_row,
    } = op
    else {
        return Err(PythonComputeError::ExpectedPythonCompute.into());
    };
    if *contract_version != CONTRACT_VERSION || language_profile != LANGUAGE_PROFILE {
        return Err(PythonComputeError::UnsupportedContractVersion.into());
    }
    let ctx =
        es.contexts_by_entry
            .get(entry_id)
            .ok_or_else(|| PythonComputeError::CatalogNotLoaded {
                entry_id: entry_id.clone(),
            })?;
    if ctx.cgs.catalog_cgs_hash_hex() != *catalog_hash {
        return Err(PythonComputeError::CatalogPinMismatch {
            entry_id: entry_id.clone(),
        }
        .into());
    }
    let exposure = es
        .teaching_exposure
        .as_ref()
        .ok_or(PythonComputeError::SessionSymbolsMissing)?;
    let symbols = exposure.to_symbol_map();
    let token = entity
        .as_ref()
        .map(|entity| symbols.entity_sym_for(entry_id, entity))
        .unwrap_or_default();
    let checked = PreparedCompute::prepare_typed(
        source,
        &ctx.cgs,
        entry_id,
        symbols.as_ref(),
        input_schema.as_ref().map(|s| (s, token.as_str())),
        &return_domains(es)?,
        if *per_row {
            ComputeInputMode::Singleton
        } else {
            ComputeInputMode::Collection
        },
    )?;
    if checked.contract.as_ref().map(|c| c.owner.entity.as_str()) != entity.as_deref() {
        return Err(PythonComputeError::DeclaredOwnerMismatch.into());
    }
    if checked.per_row != *per_row {
        return Err(PythonComputeError::StoredInputModeMismatch.into());
    }
    if &checked.output != output_type {
        return Err(PythonComputeError::StoredOutputTypeMismatch.into());
    }
    Ok(checked)
}

pub(crate) fn validate_plan_compute(
    es: &crate::execute_session::ExecuteSession,
    compute: &crate::plasm_plan::ValidatedComputeNode,
    nodes: &[crate::plasm_plan::ValidatedPlanNode],
) -> Result<(), PythonComputeRejection> {
    use crate::plasm_plan::ValidatedPlanNode;
    let checked = check_op(es, &compute.compute.op)?;
    let source_mode = if crate::plasm_plan::scoped_capture_permits_singleton(
        nodes,
        compute.compute.source.as_str(),
    ) {
        ComputeInputMode::Singleton
    } else {
        ComputeInputMode::Collection
    };
    if checked.per_row != source_mode.per_row() {
        return Err(PythonComputeError::InputModeMismatch.into());
    }
    if compute.compute.schema
        != plasm_core::plasm_monad::SyntheticResultSchema::for_value(checked.output.clone())
            .map_err(PythonComputeRejection::from)?
    {
        return Err(PythonComputeError::OutputSchemaMismatch.into());
    }
    if checked.per_row && compute.result_shape != plasm_core::plasm_monad::ResultShape::List {
        return Err(PythonComputeError::PerRowRenderRequiresRowset.into());
    }
    if !checked.per_row && compute.result_shape != plasm_core::plasm_monad::ResultShape::Single {
        return Err(PythonComputeError::ReductionRequiresSingleton.into());
    }

    if let plasm_core::plasm_monad::ComputeOp::Python {
        input_schema: Some(schema),
        ..
    } = &compute.compute.op
    {
        let source = nodes
            .iter()
            .find(|n| n.id().as_str() == compute.compute.source.as_str())
            .ok_or(PythonComputeError::SourceNodeAbsent)?;
        if let ValidatedPlanNode::Compute(source) = source {
            // Row-preserving operators retain navigation capabilities, but those
            // capabilities are not implicitly materialized compute values.
            let preserves_entity = matches!(
                source.compute.op,
                plasm_core::plasm_monad::ComputeOp::Limit { .. }
                    | plasm_core::plasm_monad::ComputeOp::Filter { .. }
                    | plasm_core::plasm_monad::ComputeOp::Sort { .. }
                    | plasm_core::plasm_monad::ComputeOp::DedupeBy { .. }
            );
            if !preserves_entity && &source.compute.schema != schema {
                return Err(PythonComputeError::InputSchemaMismatch.into());
            }
        }
        if matches!(
            source,
            ValidatedPlanNode::Data(_) | ValidatedPlanNode::Derive(_)
        ) {
            let actual =
                crate::map_body_schema::row_contract(es, nodes, compute.compute.source.as_str())
                    .map_err(PythonComputeRejection::from)?;
            if plasm_core::plasm_monad::SyntheticResultSchema::for_value(actual)
                .map_err(PythonComputeRejection::from)?
                .row_contract()
                .map_err(PythonComputeRejection::from)?
                != schema
                    .row_contract()
                    .map_err(PythonComputeRejection::from)?
            {
                return Err(PythonComputeError::StructuralInputSchemaMismatch.into());
            }
        }
        if let Some(contract) = &checked.contract {
            validate_source_owner(nodes, compute.compute.source.as_str(), &contract.owner)
                .map_err(PythonComputeRejection::from)?;
        }
        for field in checked
            .row_fields
            .as_ref()
            .into_iter()
            .flat_map(|f| f.keys())
        {
            let actual = source_field_kind(es, nodes, compute.compute.source.as_str(), field, 0)
                .map_err(PythonComputeRejection::from)?;
            if Some(actual)
                != checked
                    .row_fields
                    .as_ref()
                    .and_then(|f| f.get(field))
                    .cloned()
            {
                return Err(PythonComputeError::DerivedInputTypeMismatch {
                    field: field.clone(),
                }
                .into());
            }
        }
        return Ok(());
    }
    let contract = checked
        .contract
        .as_ref()
        .ok_or(PythonComputeError::ExplicitInputSchemaRequired)?;
    let mut source_id = compute.compute.source.as_str();
    let owner = loop {
        let source = nodes
            .iter()
            .find(|node| node.id().as_str() == source_id)
            .ok_or_else(|| PythonComputeError::SourceNodeMissing {
                source_id: source_id.to_owned(),
            })?;
        match source {
            ValidatedPlanNode::Surface(source) => {
                if !source.projection.is_empty()
                    && contract
                        .fields
                        .keys()
                        .any(|name| !source.projection.contains(name))
                {
                    let field = contract
                        .fields
                        .keys()
                        .find(|name| !source.projection.contains(*name))
                        .expect("missing projected field was checked");
                    return Err(PythonComputeError::ProjectionOmitsRequiredValue {
                        field: field.clone(),
                    }
                    .into());
                }
                break source
                    .qualified_entity
                    .as_ref()
                    .ok_or(PythonComputeError::CatalogOwnershipRequired)?;
            }
            ValidatedPlanNode::RelationTraversal(source) => {
                if let Some(fields) = &source.relation.ir.projection {
                    if let Some(field) = contract.fields.keys().find(|name| !fields.contains(*name))
                    {
                        return Err(PythonComputeError::ProjectionOmitsRequiredValue {
                            field: field.clone(),
                        }
                        .into());
                    }
                }
                break &source.relation.target;
            }
            ValidatedPlanNode::Compute(source) => {
                use plasm_core::plasm_monad::ComputeOp;
                match &source.compute.op {
                    ComputeOp::Project { fields } => {
                        if contract.fields.keys().any(|name| {
                            !fields
                                .iter()
                                .any(|(out, path)| out.as_str() == name && path.dotted() == *name)
                        }) {
                            let field = contract
                                .fields
                                .keys()
                                .find(|name| {
                                    !fields.iter().any(|(out, path)| {
                                        out.as_str() == *name && path.dotted() == **name
                                    })
                                })
                                .expect("missing projected field was checked");
                            return Err(PythonComputeError::ProjectionDropsConsumedField {
                                field: field.clone(),
                            }
                            .into());
                        }
                    }
                    ComputeOp::Filter { .. }
                    | ComputeOp::Sort { .. }
                    | ComputeOp::Limit { .. }
                    | ComputeOp::DedupeBy { .. } => {}
                    _ => return Err(PythonComputeError::CatalogRowsRequired.into()),
                }
                source_id = &source.compute.source;
            }
            ValidatedPlanNode::IterateUntil(source) => {
                source_id = source.source.as_str();
            }
            ValidatedPlanNode::ForEach(source) => {
                for fields in [&source.projection, &source.effect_template.projection] {
                    if !fields.is_empty()
                        && contract.fields.keys().any(|name| !fields.contains(name))
                    {
                        let field = contract
                            .fields
                            .keys()
                            .find(|name| !fields.contains(*name))
                            .expect("missing projected field was checked");
                        return Err(PythonComputeError::FanoutProjectionOmitsRequiredValue {
                            field: field.clone(),
                        }
                        .into());
                    }
                }
                break &source.effect_template.qualified_entity;
            }
            _ => return Err(PythonComputeError::TypedEntityRowsRequired.into()),
        }
    };
    if owner.entry_id != contract.owner.entry_id.as_str()
        || owner.entity != contract.owner.entity.as_str()
    {
        return Err(PythonComputeError::SourceOwnershipMismatch.into());
    }
    Ok(())
}

/// Validate logical membership independently of delivery. The compute ingress also
/// verifies that supplied typed rows cover every recorded occurrence.
pub(crate) fn require_complete_collection(
    result: &plasm_runtime::ExecutionResult,
) -> Result<(), plasm_core::collection_codec::CollectionFault> {
    use plasm_core::collection_codec::{CollectionCodec, Demand, RecordingCodec};
    RecordingCodec::new().materialize(result.collection.membership(), Demand::Whole)?;
    Ok(())
}

/// Cancellation drops the process session; its supervisor kills and reaps before releasing capacity.
pub(crate) async fn run_worker(
    pool: &crate::python_pool::PythonPool,
    checked: PreparedCompute,
    owner: EntityBinding,
    membership: &RecordedCollection<plasm_core::Ref>,
    rows: Vec<ValueRow>,
    scope: Option<&crate::operation::ExecutionScope>,
) -> Result<Value, plasm_runtime::ExecutionFailure> {
    await_checked(scope, checked.run(pool, &owner, membership, &rows)).await
}

pub(crate) async fn await_checked<T, E: From<plasm_runtime::ExecutionFailure>>(
    scope: Option<&crate::operation::ExecutionScope>,
    future: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let Some(scope) = scope else {
        return future.await;
    };
    scope.check()?;
    tokio::pin!(future);
    let token = scope.cancellation_token();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(10));
    loop {
        tokio::select! {
            biased;
            _ = token.cancelled() => return Err(E::from(crate::operation::ExecutionScope::cancelled_failure())),
            _ = tick.tick() => scope.check()?,
            value = &mut future => {
                // Preserve a completed failure and its effect evidence before checking
                // cancellation; cancellation cannot replace an observed failure.
                let value = value?;
                scope.check()?;
                return Ok(value);
            }
        }
    }
}

pub(crate) fn validate_input_budget(rows: &[ValueRow]) -> Result<(), ComputeInputBudgetError> {
    if rows.len() > MAX_INPUT_ROWS {
        return Err(ComputeInputBudgetError::RowCountExceeded);
    }
    let mut remaining = 1_048_576;
    for row in rows {
        plasm_core::charge_value_budget(row, &mut remaining)?;
    }

    Ok(())
}

#[derive(Default)]
pub(crate) struct ReturnDomains {
    types: BTreeMap<String, plasm_core::value_contract::ValueContract>,
    catalogs: BTreeMap<String, std::sync::Arc<CGS>>,
}
pub(crate) fn return_domains(
    es: &crate::execute_session::ExecuteSession,
) -> Result<ReturnDomains, PythonComputeRejection> {
    let exposure = es
        .teaching_exposure
        .as_ref()
        .ok_or(PythonComputeError::SessionSymbolsMissing)?;
    Ok(ReturnDomains {
        types: plasm_core::prompt_render::python::prepare_python_teaching_wave(
            exposure,
            &es.python_teaching,
        )?
        .value_contracts,
        catalogs: es
            .contexts_by_entry
            .iter()
            .map(|(entry, context)| (entry.clone(), context.cgs.clone()))
            .collect(),
    })
}

pub(crate) use inference::{branch_contracts, observation_paths};

fn validate_collection_input(
    membership: &RecordedCollection<plasm_core::Ref>,
    row_count: usize,
) -> Result<(), CollectionFault> {
    let rows = RecordingCodec::new().materialize(membership, Demand::Whole)?;
    if rows.len() != row_count {
        return Err(CollectionFault::Conservation);
    }
    Ok(())
}

#[cfg(test)]
mod typed_helper_input_error_tests {
    use super::*;

    #[test]
    fn helper_input_contract_failures_are_semantic() {
        use crate::program_rejection::PythonProgramError;
        use plasm_core::value_contract::ValueContract;

        assert!(matches!(
            inferred_helper_input_annotation(
                &ValueContract::scalar(FieldType::Array),
                false,
                false,
            ),
            Err(PythonProgramError::HelperInputRequiresTemporalOrArrayContract)
        ));
        assert!(matches!(
            inferred_helper_input_annotation(
                &ValueContract::record(BTreeMap::new(), Default::default()),
                true,
                false,
            ),
            Err(PythonProgramError::HelperScalarCellValueMissing)
        ));
    }
}
