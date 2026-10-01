mod admission;
#[cfg(test)]
mod analysis_tests;
mod arguments;
mod inference;
mod returns;
mod upstream;
pub(crate) use upstream::admit_bundle;
mod schema;
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

pub(crate) const LANGUAGE_PROFILE: &str =
    "monty-e007685fbb06494c13b9a7b3fede9f8e6a54a2be-typed-v9-money-v2-branches-v2";

pub(crate) const MAX_INPUT_ROWS: usize = 256;

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
    ) -> Result<Self, String> {
        let owner = symbols
            .resolve_session_entity(token)
            .map_err(|e| e.to_string())?;
        if owner.entry_id.as_str() != entry {
            return Err("value contract catalog ownership mismatch".into());
        }
        let entity = cgs
            .get_entity(owner.entity.as_str())
            .ok_or("unknown entity")?;
        let mut fields = BTreeMap::new();
        for field in entity.fields.values() {
            let value = field.named_value(cgs).map_err(|e| e.to_string())?;
            let mut value_type = plasm_core::value_contract::ValueContract::from_domain(
                cgs,
                entry,
                field.kind.registry_key(),
            )?;
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
    pub fn declaration(&self, symbols: &plasm_core::SymbolMap) -> Result<String, String> {
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
                        .map_err(|e| e.to_string())?;
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
                None => return Err("missing allocated value symbol".into()),
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
    ) -> Result<crate::python_pool::TypedRecords, String> {
        if owner != &self.owner {
            return Err("compute input catalog/entity ownership mismatch".into());
        }
        validate_collection_input(membership, rows.len()).map_err(|e| e.to_string())?;
        if rows.len() > MAX_INPUT_ROWS {
            return Err("compute input row budget exceeded".into());
        }
        validate_input_budget(rows)?;
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
                let value = contract.value_type.observed_value(
                    value,
                    &self.cgs,
                    self.owner.entry_id.as_str(),
                )?;
                contract.value_type.validate(
                    &value,
                    &self.cgs,
                    self.owner.entry_id.as_str(),
                    field,
                )?;
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
) -> Result<String, String> {
    let start = def
        .body
        .first()
        .ok_or("empty compute body")?
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

pub struct PreparedCompute {
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

impl PreparedCompute {
    pub fn prepare(
        source: &str,
        cgs: &CGS,
        entry: &str,
        symbols: &dyn SymbolResolve,
    ) -> Result<Self, String> {
        Self::prepare_input(source, cgs, entry, symbols, None)
    }

    pub(crate) fn prepare_input(
        source: &str,
        cgs: &CGS,
        entry: &str,
        symbols: &dyn SymbolResolve,
        rows: Option<(&SyntheticResultSchema, &str)>,
    ) -> Result<Self, String> {
        Self::prepare_typed(source, cgs, entry, symbols, rows, &ReturnDomains::default())
    }

    pub(crate) fn prepare_typed(
        source: &str,
        cgs: &CGS,
        entry: &str,
        symbols: &dyn SymbolResolve,
        rows: Option<(&SyntheticResultSchema, &str)>,
        domains: &ReturnDomains,
    ) -> Result<Self, String> {
        if source.len() > 4096 {
            return Err("compute source budget exceeded".into());
        }
        let ast = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
        let (imports, suite) = crate::python_datetime::Imports::split(source, ast.suite())?;
        let [Stmt::FunctionDef(def)] = suite else {
            return Err("expected one compute function".into());
        };
        if def.is_async
            || def.type_params.is_some()
            || def.decorator_list.len() != 1
            || name(&def.decorator_list[0].expression) != Some("compute")
        {
            return Err("expected a synchronous @compute function".into());
        }
        let p = &def.parameters;
        if !p.posonlyargs.is_empty()
            || !p.kwonlyargs.is_empty()
            || p.vararg.is_some()
            || p.kwarg.is_some()
            || p.args.len() != 1
            || p.args[0].default.is_some()
        {
            return Err("compute expects one required positional input".into());
        }
        let param = &p.args[0].parameter;
        let ann = param
            .annotation
            .as_deref()
            .ok_or("missing input annotation")?;
        let value = match ann {
            Expr::Subscript(s) if name(&s.value) == Some("list") => &*s.slice,
            _ => ann,
        };
        let row_annotation = arguments::is_row(value);
        let token = if let Some((_, token)) = rows {
            if row_annotation && name(value) != Some("Row") {
                let annotated =
                    name(entity_record_argument(value)?).ok_or("expected entity symbol")?;
                if annotated != token {
                    return Err("compute annotation does not match source entity".into());
                }
            }
            (!token.is_empty()).then_some(token)
        } else {
            Some(name(entity_record_argument(value)?).ok_or("expected entity symbol")?)
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
                    return Err("synthetic type summary differs from recursive contract".into());
                }
            }
        }
        let fields = if let Some((schema, _)) = rows {
            schema
                .fields
                .iter()
                .map(|f| {
                    let ty = f.value_type.clone().ok_or_else(|| {
                        format!(
                            "Python input field {} requires a recursive contract",
                            f.name
                        )
                    })?;
                    Ok((f.name.to_string(), ty))
                })
                .collect::<Result<BTreeMap<_, _>, String>>()?
        } else {
            contract
                .as_ref()
                .ok_or("nominal compute requires an entity")?
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
                .ok_or("nominal compute requires an entity")?
                .fields
                .retain(|_, field| field.domain.is_some());
        }
        let input = plasm_core::value_contract::ValueContract::record(
            fields.clone(),
            rows.map(|(s, _)| s.optional_fields.clone())
                .unwrap_or_default(),
        );
        let mut return_types = domains.types.clone();
        for (local, canonical) in &imports.bindings {
            if let Some(kind) = canonical
                .strip_prefix("datetime.")
                .and_then(plasm_core::temporal_value::TemporalKind::parse)
            {
                return_types.insert(local.clone(), kind.contract());
            } else if canonical == "datetime" {
                return_types.insert(
                    local.clone(),
                    plasm_core::value_contract::ValueContract::record(
                        ["date", "datetime", "time", "timedelta", "timezone"]
                            .into_iter()
                            .map(|name| {
                                (
                                    name.into(),
                                    plasm_core::temporal_value::TemporalKind::parse(name)
                                        .unwrap()
                                        .contract(),
                                )
                            })
                            .collect(),
                        Default::default(),
                    ),
                );
            }
        }
        let resolved_domains = ReturnDomains {
            types: return_types,
            catalogs: domains.catalogs.clone(),
        };
        let argument =
            arguments::resolve(ann, &input, &fields, &resolved_domains, cgs, entry, symbols)?;
        let per_row = argument.per_row;
        let inferred = if def.returns.is_none() {
            let [Stmt::Return(ret)] = def.body.as_slice() else {
                return Err("compute without an annotation requires one return expression".into());
            };
            let expr = ret
                .value
                .as_deref()
                .ok_or("compute requires a returned value")?;
            let input_type = if argument.field.is_some() {
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
            Some(inference::infer(
                &source[expr.start().to_usize()..expr.end().to_usize()],
                param.name.as_str(),
                &input_type,
                &imports.source,
            )?)
        } else {
            None
        };
        let output = if let Some(output) = inferred {
            output
        } else if let Some(kind) = def
            .returns
            .as_deref()
            .and_then(|annotation| imports.path(annotation))
            .and_then(|p| {
                p.strip_prefix("datetime.")
                    .and_then(plasm_core::temporal_value::TemporalKind::parse)
            })
        {
            kind.contract()
        } else {
            returns::resolve(
                def.returns
                    .as_deref()
                    .ok_or("compute requires a return annotation")?,
                &input,
                &resolved_domains.types,
                &domains.catalogs,
                cgs,
                entry,
                symbols,
            )?
        };
        let mut stubs = upstream::stubs_in(&fields, cgs, &domains.catalogs)?;
        upstream::domain_aliases(&domains.types, cgs, &domains.catalogs, &mut stubs)?;
        let output_type = upstream::output_type(&output, cgs, &domains.catalogs, &mut stubs)?;
        let body = definition_body(source, def)?;
        let argument_type = if argument.field.is_some() {
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
        upstream::validate_policy(&definition)?;
        let executable = format!("{}\n{}({})", definition, def.name, argument.expression());
        Ok(Self {
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
                return Err("compute input catalog/entity ownership mismatch".into());
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
            )?
        } else {
            self.contract
                .as_ref()
                .ok_or("missing nominal input contract")?
                .materialize(owner, membership, rows)?
        };
        let types = match &self.row_fields {
            Some(fields) => fields.clone(),
            None => self
                .contract
                .as_ref()
                .ok_or("missing nominal input contract")?
                .fields
                .iter()
                .map(|(k, f)| (k.clone(), f.value_type.clone()))
                .collect(),
        };
        self.argument
            .validate(&input, &self.context, &self.entry, &self.catalogs)?;
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
    ) -> Result<Value, String> {
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
                            .ok_or("money catalog is absent")?
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
                plasm_core::temporal_value::encode(&value, wire)
            }
            ValueShape::Array { element } => value
                .as_array()
                .ok_or("expected array output")?
                .iter()
                .map(|v| self.encode_domain_output(v.clone(), element))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } => value
                .as_object()
                .ok_or("expected record output")?
                .iter()
                .map(|(k, v)| {
                    Ok((
                        k.clone(),
                        self.encode_domain_output(
                            v.clone(),
                            fields.get(k).ok_or("unknown output field")?,
                        )?,
                    ))
                })
                .collect::<Result<indexmap::IndexMap<_, _>, String>>()
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
                Err("unmatched output union".into())
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

fn entity_record_argument(expr: &Expr) -> Result<&Expr, String> {
    match expr {
        Expr::Subscript(s) if name(&s.value).is_some_and(is_entity_record_type) => Ok(&s.slice),
        _ => Err("expected Value[eN] or Row[eN] annotation".into()),
    }
}

/// Recheck a serialized compute against the owning catalog and current session symbols.
/// This compiles code but does not execute it or manufacture sample values.
pub(crate) fn check_op(
    es: &crate::execute_session::ExecuteSession,
    op: &plasm_core::plasm_monad::ComputeOp,
) -> Result<PreparedCompute, String> {
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
        return Err("expected Python compute".into());
    };
    if *contract_version != 9 || language_profile != LANGUAGE_PROFILE {
        return Err("unsupported Python compute contract version".into());
    }
    let ctx = es
        .contexts_by_entry
        .get(entry_id)
        .ok_or("Python compute catalog is not loaded")?;
    if ctx.cgs.catalog_cgs_hash_hex() != *catalog_hash {
        return Err("Python compute catalog pin mismatch".into());
    }
    let exposure = es
        .teaching_exposure
        .as_ref()
        .ok_or("Python compute requires session symbols")?;
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
    )?;
    if checked.contract.as_ref().map(|c| c.owner.entity.as_str()) != entity.as_deref() {
        return Err("Python compute annotated entity does not match declared owner".into());
    }
    if checked.per_row != *per_row {
        return Err("Python compute input mode differs from its annotation and schema".into());
    }
    if &checked.output != output_type {
        return Err("Python output type differs from checked return contract".into());
    }
    Ok(checked)
}

pub(crate) fn validate_plan_compute(
    es: &crate::execute_session::ExecuteSession,
    compute: &crate::plasm_plan::ValidatedComputeNode,
    nodes: &[crate::plasm_plan::ValidatedPlanNode],
) -> Result<(), String> {
    use crate::plasm_plan::ValidatedPlanNode;
    let checked = check_op(es, &compute.compute.op)?;
    if compute.compute.schema
        != plasm_core::plasm_monad::SyntheticResultSchema::for_value(checked.output.clone())?
    {
        return Err("Python output schema differs from declared return contract".into());
    }
    if checked.per_row && compute.result_shape != plasm_core::plasm_monad::ResultShape::List {
        return Err("per-row Python rendering must declare a rowset result".into());
    }
    if !checked.per_row && compute.result_shape != plasm_core::plasm_monad::ResultShape::Single {
        return Err("Python reduction must declare a singleton result".into());
    }

    if let plasm_core::plasm_monad::ComputeOp::Python {
        input_schema: Some(schema),
        ..
    } = &compute.compute.op
    {
        let source = nodes
            .iter()
            .find(|n| n.id().as_str() == compute.compute.source.as_str())
            .ok_or("Python source absent")?;
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
                return Err("Python input schema differs from its source".into());
            }
        }
        if matches!(
            source,
            ValidatedPlanNode::Data(_) | ValidatedPlanNode::Derive(_)
        ) {
            let actual =
                crate::map_body_schema::row_contract(es, nodes, compute.compute.source.as_str())?;
            if plasm_core::plasm_monad::SyntheticResultSchema::for_value(actual)?.row_contract()?
                != schema.row_contract()?
            {
                return Err("Python input schema differs from its structural source".into());
            }
        }
        if let Some(contract) = &checked.contract {
            validate_source_owner(nodes, compute.compute.source.as_str(), &contract.owner)?;
        }
        for field in checked
            .row_fields
            .as_ref()
            .into_iter()
            .flat_map(|f| f.keys())
        {
            let actual = source_field_kind(es, nodes, compute.compute.source.as_str(), field, 0)?;
            if Some(actual)
                != checked
                    .row_fields
                    .as_ref()
                    .and_then(|f| f.get(field))
                    .cloned()
            {
                return Err(format!(
                    "Python input field {field} differs from its derived type"
                ));
            }
        }
        return Ok(());
    }
    let contract = checked
        .contract
        .as_ref()
        .ok_or("structural compute requires an explicit input schema")?;
    let mut source_id = compute.compute.source.as_str();
    let owner = loop {
        let source = nodes
            .iter()
            .find(|node| node.id().as_str() == source_id)
            .ok_or("Python compute source is absent")?;
        match source {
            ValidatedPlanNode::Surface(source) => {
                if !source.projection.is_empty()
                    && contract
                        .fields
                        .keys()
                        .any(|name| !source.projection.contains(name))
                {
                    return Err(
                        "Python compute input projection omits a required value field".into(),
                    );
                }
                break source
                    .qualified_entity
                    .as_ref()
                    .ok_or("Python compute requires catalog ownership")?;
            }
            ValidatedPlanNode::RelationTraversal(source) => {
                if source
                    .relation
                    .ir
                    .projection
                    .as_ref()
                    .is_some_and(|fields| contract.fields.keys().any(|name| !fields.contains(name)))
                {
                    return Err(
                        "Python compute input projection omits a required value field".into(),
                    );
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
                            return Err("Python compute projection must preserve each consumed catalog field".into());
                        }
                    }
                    ComputeOp::Filter { .. }
                    | ComputeOp::Sort { .. }
                    | ComputeOp::Limit { .. }
                    | ComputeOp::DedupeBy { .. } => {}
                    _ => {
                        return Err(
                            "Python compute requires rows preserving the annotated catalog type"
                                .into(),
                        )
                    }
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
                        return Err(
                            "Python compute fanout projection omits a required value field".into(),
                        );
                    }
                }
                break &source.effect_template.qualified_entity;
            }
            _ => return Err("Python compute requires typed entity rows".into()),
        }
    };
    if owner.entry_id != contract.owner.entry_id.as_str()
        || owner.entity != contract.owner.entity.as_str()
    {
        return Err("Python compute source catalog/entity ownership mismatch".into());
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

pub(crate) async fn await_checked<T, E: From<String>>(
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
            _ = token.cancelled() => return Err(E::from("operation cancelled".to_string())),
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

pub(crate) fn validate_input_budget(rows: &[ValueRow]) -> Result<(), String> {
    if rows.len() > MAX_INPUT_ROWS {
        return Err("compute input row budget exceeded".into());
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
) -> Result<ReturnDomains, String> {
    let exposure = es
        .teaching_exposure
        .as_ref()
        .ok_or("Python compute requires session symbols")?;
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
