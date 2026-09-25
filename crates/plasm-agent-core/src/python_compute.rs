#[cfg(test)]
mod boolean_tests;
mod schema;
use schema::{kind_from_schema, materialize_rows, source_field_kind, validate_source_owner};
// Restricted pure Python compute boundary for the experimental DAG slice.
use plasm_core::plasm_monad::{SyntheticResultSchema, SyntheticValueKind};
use plasm_core::symbol_tuning::{EntityBinding, SymbolResolve};
use plasm_core::{FieldType, CGS};
use plasm_runtime::ResultCoverage;
use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

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
        coverage: ResultCoverage,
        rows: &[Value],
    ) -> Result<crate::python_pool::TypedRecords, String> {
        if owner != &self.owner {
            return Err("compute input catalog/entity ownership mismatch".into());
        }
        if coverage != ResultCoverage::Complete {
            return Err("compute needs complete collection coverage".into());
        }
        if rows.len() > MAX_INPUT_ROWS {
            return Err("compute input row budget exceeded".into());
        }
        if serde_json::to_vec(rows).map_err(|e| e.to_string())?.len() > 1_048_576 {
            return Err("compute input byte budget exceeded".into());
        }
        let mut values = Vec::with_capacity(rows.len());
        for row in rows {
            let mut attrs = BTreeMap::new();
            for (field, contract) in &self.fields {
                if contract.domain.is_none() && row.get(field).is_none() {
                    return Err(format!("{field}: relation observation is unavailable; traverse the relation explicitly"));
                }
                let value = row.get(field).unwrap_or(&Value::Null);
                if value.is_null() && !contract.required {
                    attrs.insert(field.clone(), Value::Null);
                    continue;
                }
                contract.value_type.validate(
                    value,
                    &self.cgs,
                    self.owner.entry_id.as_str(),
                    field,
                )?;
                attrs.insert(field.clone(), value.clone());
            }
            values.push(attrs);
        }
        Ok(values)
    }
}

fn observed_relation_type(
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

pub struct CheckedCompute {
    pub contract: ValueContract,
    executable: String,
    row_fields: Option<BTreeMap<String, Kind>>,
    pub(crate) per_row: bool,
}

impl CheckedCompute {
    pub fn compile(
        source: &str,
        cgs: &CGS,
        entry: &str,
        symbols: &dyn SymbolResolve,
    ) -> Result<Self, String> {
        Self::compile_input(source, cgs, entry, symbols, None, false)
    }

    pub(crate) fn compile_input(
        source: &str,
        cgs: &CGS,
        entry: &str,
        symbols: &dyn SymbolResolve,
        rows: Option<(&SyntheticResultSchema, &str)>,
        per_row: bool,
    ) -> Result<Self, String> {
        if source.len() > 4096 {
            return Err("compute source budget exceeded".into());
        }
        let ast = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
        let [Stmt::FunctionDef(def)] = ast.suite().as_slice() else {
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
            return Err("slice expects one required positional collection".into());
        }
        let param = &p.args[0].parameter;
        let ann = param
            .annotation
            .as_deref()
            .ok_or("missing input annotation")?;
        let value = if per_row {
            ann
        } else {
            subscript(ann, "list")?
        };
        let token = if let Some((_, token)) = rows {
            if name(value) != Some("Row") {
                return Err("inferred row compute requires Row or list[Row]".into());
            }
            token
        } else {
            name(subscript(value, "Value")?).ok_or("expected entity symbol")?
        };
        let mut contract = ValueContract::from_cgs(cgs, entry, symbols, token)?;
        if def.returns.as_deref().and_then(name) != Some("str") {
            return Err("slice requires -> str".into());
        }
        let [Stmt::Return(ret)] = def.body.as_slice() else {
            return Err("slice admits one pure return expression".into());
        };
        let expr = ret.value.as_deref().ok_or("missing return value")?;
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
        let mut fields = if let Some((schema, _)) = rows {
            schema
                .fields
                .iter()
                .filter_map(|f| {
                    f.value_type
                        .clone()
                        .map(Kind::Typed)
                        .or_else(|| kind_from_schema(f.value_kind))
                        .map(|k| (f.name.to_string(), k))
                })
                .collect()
        } else {
            contract
                .fields
                .iter()
                .map(|(k, f)| (k.clone(), Kind::Typed(f.value_type.clone())))
                .collect::<BTreeMap<_, _>>()
        };
        let mut env = BTreeMap::from([(
            param.name.to_string(),
            if per_row { Kind::Record } else { Kind::Rows },
        )]);
        let mut used = BTreeSet::new();
        if infer(expr, &mut env, &fields, &mut used)? != Kind::String {
            return Err("compute output: expected str".into());
        }
        contract.fields.retain(|name, _| used.contains(name));
        fields.retain(|name, _| used.contains(name));
        // Strip only the DSL signature/decorator. Preserve the admitted body source exactly.
        // No domain symbol or host function is supplied to the interpreter.
        let body = &source[ret.range().start().to_usize()..ret.range().end().to_usize()];
        let executable = format!(
            "def {}({}):\n    {}\n{}({})",
            def.name,
            param.name,
            body,
            def.name,
            if per_row { "__input[0]" } else { "__input" }
        );
        Ok(Self {
            contract,
            executable,
            row_fields: rows.map(|_| fields),
            per_row,
        })
    }

    pub async fn run(
        &self,
        pool: &crate::python_pool::PythonPool,
        owner: &EntityBinding,
        coverage: ResultCoverage,
        rows: &[Value],
    ) -> Result<String, String> {
        if owner.entry_id != self.contract.owner.entry_id
            || (self.row_fields.is_none() && owner != &self.contract.owner)
        {
            return Err("compute input catalog/entity ownership mismatch".into());
        }
        if coverage != ResultCoverage::Complete {
            return Err("compute needs complete collection coverage".into());
        }
        let input = if let Some(fields) = &self.row_fields {
            materialize_rows(
                fields,
                rows,
                &self.contract.cgs,
                self.contract.owner.entry_id.as_str(),
            )?
        } else {
            self.contract.materialize(owner, coverage, rows)?
        };
        let types = match &self.row_fields {
            Some(fields) => fields
                .iter()
                .filter_map(|(k, v)| {
                    if let Kind::Typed(t) = v {
                        Some((k.clone(), t.clone()))
                    } else {
                        None
                    }
                })
                .collect(),
            None => self
                .contract
                .fields
                .iter()
                .map(|(k, f)| (k.clone(), f.value_type.clone()))
                .collect(),
        };
        pool.compute_typed(self.executable.clone(), input, &types)
            .await
    }
}

fn name(expr: &Expr) -> Option<&str> {
    if let Expr::Name(n) = expr {
        Some(n.id.as_str())
    } else {
        None
    }
}
fn subscript<'a>(expr: &'a Expr, head: &str) -> Result<&'a Expr, String> {
    if let Expr::Subscript(s) = expr {
        if name(&s.value) == Some(head) {
            return Ok(&s.slice);
        }
    }
    Err(format!("expected {head}[...]"))
}
#[derive(Clone, PartialEq, Eq)]
enum Kind {
    Typed(plasm_core::value_contract::ValueContract),
    Rows,
    Record,
    Strings,
    String,
    OptionalString,
    Integer,
    Number,
    Null,
    Boolean,
    Scalars,
    ScalarUnion(Vec<Kind>),
}

fn infer(
    expr: &Expr,
    env: &mut BTreeMap<String, Kind>,
    contract: &BTreeMap<String, Kind>,
    used: &mut BTreeSet<String>,
) -> Result<Kind, String> {
    let kind = infer_inner(expr, env, contract, used)?;
    if let Kind::Typed(t) = &kind {
        use plasm_core::value_contract::ValueShape;
        if let ValueShape::Scalar { field_type } = &t.shape {
            let scalar = match field_type {
                FieldType::String | FieldType::Select | FieldType::Uuid | FieldType::DigitId => {
                    Some(if t.nullable {
                        Kind::OptionalString
                    } else {
                        Kind::String
                    })
                }
                FieldType::Integer => Some(Kind::Integer),
                FieldType::Number => Some(Kind::Number),
                FieldType::Boolean => Some(Kind::Boolean),
                _ => None,
            };
            if let Some(s) = scalar {
                return Ok(s);
            }
        }
    }
    Ok(kind)
}

// A closed, intentionally small expression checker; unsupported Python is rejected.
// A general pure-Python checker is separate work, not implied by this test.
fn infer_inner(
    expr: &Expr,
    env: &mut BTreeMap<String, Kind>,
    contract: &BTreeMap<String, Kind>,
    used: &mut BTreeSet<String>,
) -> Result<Kind, String> {
    match expr {
        Expr::Name(n) => env
            .get(n.id.as_str())
            .cloned()
            .ok_or_else(|| format!("undeclared compute name {}", n.id)),
        Expr::StringLiteral(_) => Ok(Kind::String),
        Expr::NumberLiteral(number) => match number.value {
            ruff_python_ast::Number::Int(_) => Ok(Kind::Integer),
            ruff_python_ast::Number::Float(value) if value.is_finite() => Ok(Kind::Number),
            _ => Err("compute requires a finite real numeric literal".into()),
        },
        Expr::UnaryOp(unary)
            if matches!(
                unary.op,
                ruff_python_ast::UnaryOp::UAdd | ruff_python_ast::UnaryOp::USub
            ) && matches!(&*unary.operand, Expr::NumberLiteral(_)) =>
        {
            infer(&unary.operand, env, contract, used)
        }
        Expr::BooleanLiteral(_) => Ok(Kind::Boolean),
        Expr::NoneLiteral(_) => Ok(Kind::Null),
        Expr::BoolOp(boolean) => {
            let mut alternatives = Vec::new();
            for value in &boolean.values {
                let kind = infer(value, env, contract, used)?;
                if !scalar(&kind) {
                    return Err("boolean text alternatives require scalar operands".into());
                }
                alternatives.push(kind);
            }
            let mut kinds = alternatives.into_iter();
            let first = kinds.next().ok_or("empty boolean expression")?;
            Ok(
                kinds.fold(first, |left, right| match (&left, &right, boolean.op) {
                    (Kind::Null, _, ruff_python_ast::BoolOp::Or) => right,
                    (Kind::Null, _, ruff_python_ast::BoolOp::And) => Kind::Null,
                    (
                        Kind::OptionalString | Kind::String,
                        Kind::String,
                        ruff_python_ast::BoolOp::Or,
                    ) => Kind::String,
                    _ if left == right => left,
                    _ => Kind::ScalarUnion(vec![left, right]),
                }),
            )
        }
        Expr::FString(text) => {
            check_interpolations(text.value.elements(), env, contract, used)?;
            Ok(Kind::String)
        }
        Expr::BinOp(binary) => {
            let left = infer(&binary.left, env, contract, used)?;
            let right = infer(&binary.right, env, contract, used)?;
            match binary.op {
                ruff_python_ast::Operator::Add if left == Kind::String && right == Kind::String => {
                    Ok(Kind::String)
                }
                ruff_python_ast::Operator::Mod
                    if left == Kind::String
                        && (scalar(&right) || matches!(right, Kind::Strings | Kind::Scalars)) =>
                {
                    Ok(Kind::String)
                }
                _ => Err("unsupported text binary operation".into()),
            }
        }
        Expr::List(values) => infer_sequence(&values.elts, env, contract, used),
        Expr::Tuple(values) => infer_sequence(&values.elts, env, contract, used),
        Expr::Subscript(value) => {
            use plasm_core::value_contract::{ValueContract, ValueShape};
            let index_is_int = matches!(&*value.slice, Expr::NumberLiteral(n) if matches!(n.value, ruff_python_ast::Number::Int(_)));
            let key = if let Expr::StringLiteral(text) = &*value.slice {
                Some(text.value.to_str())
            } else {
                None
            };
            match infer(&value.value, env, contract, used)? {
                Kind::Rows if index_is_int => Ok(Kind::Record),
                Kind::String | Kind::Strings if index_is_int => Ok(Kind::String),
                Kind::Typed(t) if !t.nullable => match &t.shape {
                    ValueShape::Array { element } if index_is_int => {
                        Ok(Kind::Typed(*element.clone()))
                    }
                    ValueShape::Record { .. } => Err(
                        "typed records use attribute access (record.field); string-key subscripting is for JSON objects".into(),
                    ),
                    ValueShape::Scalar {
                        field_type: FieldType::MultiSelect,
                    } if index_is_int => Ok(Kind::String),
                    ValueShape::Scalar {
                        field_type: FieldType::Json | FieldType::Blob | FieldType::EntityRef { .. },
                    } if index_is_int || key.is_some() => {
                        Ok(Kind::Typed(ValueContract::scalar(FieldType::Json)))
                    }
                    ValueShape::Scalar {
                        field_type: FieldType::Money,
                    } if key == Some("__plasm_money") => Ok(Kind::String),
                    ValueShape::Scalar {
                        field_type: FieldType::Money,
                    } if key == Some("currency") => Ok(Kind::OptionalString),
                    _ => Err("index does not match value contract".into()),
                },
                _ => Err("index requires a non-null collection and a typed literal key".into()),
            }
        }
        Expr::Attribute(a) => match infer(&a.value, env, contract, used)? {
            Kind::Record => {
                used.insert(a.attr.to_string());
                contract
                    .get(a.attr.as_str())
                    .cloned()
                    .ok_or_else(|| format!("unknown value field {}", a.attr))
            }
            Kind::Typed(t) if !t.nullable => {
                if let plasm_core::value_contract::ValueShape::Record { fields } = t.shape {
                    fields
                        .get(a.attr.as_str())
                        .cloned()
                        .map(Kind::Typed)
                        .ok_or_else(|| format!("unknown record field {}", a.attr))
                } else {
                    Err("attribute requires a typed record".into())
                }
            }
            _ => Err("attribute requires a non-null typed record".into()),
        },
        Expr::Generator(g) => infer_comprehension(&g.elt, &g.generators, env, contract, used),
        Expr::ListComp(g) => infer_comprehension(&g.elt, &g.generators, env, contract, used),
        Expr::Call(call) => {
            if let Some(function) = name(&call.func) {
                if call.arguments.args.len() != 1 || !call.arguments.keywords.is_empty() {
                    return Err("text builtin requires one positional argument".into());
                }
                let arg = infer(&call.arguments.args[0], env, contract, used)?;
                return match function {
                    "str" if scalar(&arg) => Ok(Kind::String),
                    "len"
                        if matches!(arg, Kind::String | Kind::Strings | Kind::Rows)
                            || matches!(&arg, Kind::Typed(t) if !t.nullable && matches!(t.shape, plasm_core::value_contract::ValueShape::Array { .. } | plasm_core::value_contract::ValueShape::Record { .. })) =>
                    {
                        Ok(Kind::Integer)
                    }
                    _ => Err("compute calls cannot invoke domain or external functions".into()),
                };
            }
            let Expr::Attribute(method) = call.func.as_ref() else {
                return Err("compute calls cannot invoke domain or external functions".into());
            };
            if infer(&method.value, env, contract, used)? != Kind::String {
                return Err("text methods require a string receiver".into());
            }
            let args = call
                .arguments
                .args
                .iter()
                .map(|arg| infer(arg, env, contract, used))
                .collect::<Result<Vec<_>, _>>()?;
            match method.attr.as_str() {
                "format" => {
                    if !args.iter().all(scalar) {
                        return Err("format arguments must be scalars".into());
                    }
                    for keyword in &call.arguments.keywords {
                        if keyword.arg.is_none()
                            || !scalar(&infer(&keyword.value, env, contract, used)?)
                        {
                            return Err(
                                "format requires named scalar arguments without unpacking".into()
                            );
                        }
                    }
                    Ok(Kind::String)
                }
                _ if !call.arguments.keywords.is_empty() => {
                    Err("unsupported text keyword arguments".into())
                }
                "join"
                    if args == [Kind::Strings]
                        && matches!(&*method.value, Expr::StringLiteral(_)) =>
                {
                    Ok(Kind::String)
                }
                "split" if args == [Kind::String] || args.is_empty() => Ok(Kind::Strings),
                "strip" | "lstrip" | "rstrip" if args.is_empty() || args == [Kind::String] => {
                    Ok(Kind::String)
                }
                "replace" if args == [Kind::String, Kind::String] => Ok(Kind::String),
                _ => Err("unsupported compute method or argument types".into()),
            }
        }
        _ => Err(format!(
            "unsupported compute expression at byte {}",
            expr.range().start().to_usize()
        )),
    }
}

fn infer_comprehension(
    elt: &Expr,
    generators: &[ruff_python_ast::Comprehension],
    env: &mut BTreeMap<String, Kind>,
    contract: &BTreeMap<String, Kind>,
    used: &mut BTreeSet<String>,
) -> Result<Kind, String> {
    let [comp] = generators else {
        return Err("expected one collection comprehension".into());
    };
    if comp.is_async || !comp.ifs.is_empty() {
        return Err("expected a direct collection comprehension".into());
    }
    let variable = name(&comp.target).ok_or("expected comprehension variable")?;
    let mut nested = env.clone();
    let element = match infer(&comp.iter, env, contract, used)? {
        Kind::Rows if env.values().any(|k| matches!(k, Kind::Record)) => {
            return Err("repeated root collection expansion exceeds allocation contract".into())
        }
        Kind::Rows => Kind::Record,
        Kind::Strings => Kind::String,
        Kind::Typed(t) if !t.nullable => match t.shape {
            plasm_core::value_contract::ValueShape::Array { element } => Kind::Typed(*element),
            plasm_core::value_contract::ValueShape::Scalar {
                field_type: FieldType::MultiSelect,
            } => Kind::String,
            _ => return Err("comprehension requires a typed array".into()),
        },
        _ => return Err("comprehension requires a non-null array".into()),
    };
    nested.insert(variable.into(), element);
    if infer(elt, &mut nested, contract, used)? != Kind::String {
        return Err("expected string comprehension".into());
    }
    Ok(Kind::Strings)
}

fn scalar(kind: &Kind) -> bool {
    if let Kind::ScalarUnion(alternatives) = kind {
        return alternatives.iter().all(scalar);
    }
    matches!(
        kind,
        Kind::Typed(_)
            | Kind::String
            | Kind::OptionalString
            | Kind::Integer
            | Kind::Number
            | Kind::Boolean
            | Kind::Null
    )
}

fn infer_sequence(
    values: &[Expr],
    env: &mut BTreeMap<String, Kind>,
    contract: &BTreeMap<String, Kind>,
    used: &mut BTreeSet<String>,
) -> Result<Kind, String> {
    let kinds = values
        .iter()
        .map(|value| infer(value, env, contract, used))
        .collect::<Result<Vec<_>, _>>()?;
    if kinds.iter().all(|kind| *kind == Kind::String) {
        return Ok(Kind::Strings);
    }
    if kinds.iter().all(scalar) {
        return Ok(Kind::Scalars);
    }
    Err("text sequences may contain only scalar values".into())
}

fn check_interpolations<'a>(
    elements: impl Iterator<Item = &'a ruff_python_ast::InterpolatedStringElement>,
    env: &mut BTreeMap<String, Kind>,
    contract: &BTreeMap<String, Kind>,
    used: &mut BTreeSet<String>,
) -> Result<(), String> {
    for element in elements {
        if let ruff_python_ast::InterpolatedStringElement::Interpolation(value) = element {
            if !scalar(&infer(&value.expression, env, contract, used)?) {
                return Err("f-string interpolation requires a scalar".into());
            }
            if let Some(format) = &value.format_spec {
                check_interpolations(format.elements.iter(), env, contract, used)?;
            }
        }
    }
    Ok(())
}

/// Recheck a serialized compute against the owning catalog and current session symbols.
/// This compiles code but does not execute it or manufacture sample values.
pub(crate) fn check_op(
    es: &crate::execute_session::ExecuteSession,
    op: &plasm_core::plasm_monad::ComputeOp,
) -> Result<CheckedCompute, String> {
    let plasm_core::plasm_monad::ComputeOp::Python {
        source,
        entry_id,
        entity,
        catalog_hash,
        contract_version,
        input_schema,
        per_row,
    } = op
    else {
        return Err("expected Python compute".into());
    };
    if *contract_version != 3 {
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
    let token = symbols.entity_sym_for(entry_id, entity);
    let checked = CheckedCompute::compile_input(
        source,
        &ctx.cgs,
        entry_id,
        symbols.as_ref(),
        input_schema.as_ref().map(|s| (s, token.as_str())),
        *per_row,
    )?;
    if checked.contract.owner.entity.as_str() != entity {
        return Err("Python compute annotated entity does not match declared owner".into());
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
            if &source.compute.schema != schema {
                return Err("Python input schema differs from its source".into());
            }
        }
        validate_source_owner(
            nodes,
            compute.compute.source.as_str(),
            &checked.contract.owner,
        )?;
        for field in checked
            .row_fields
            .as_ref()
            .into_iter()
            .flat_map(|f| f.keys())
        {
            let actual = source_field_kind(es, nodes, compute.compute.source.as_str(), field, 0)?;
            if Some(Kind::Typed(actual))
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
    let mut source_id = compute.compute.source.as_str();
    let owner = loop {
        let source = nodes
            .iter()
            .find(|node| node.id().as_str() == source_id)
            .ok_or("Python compute source is absent")?;
        match source {
            ValidatedPlanNode::Surface(source) => {
                if !source.projection.is_empty()
                    && checked
                        .contract
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
                    .is_some_and(|fields| {
                        checked
                            .contract
                            .fields
                            .keys()
                            .any(|name| !fields.contains(name))
                    })
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
                        if checked.contract.fields.keys().any(|name| {
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
            ValidatedPlanNode::ForEach(source) => {
                for fields in [&source.projection, &source.effect_template.projection] {
                    if !fields.is_empty()
                        && checked
                            .contract
                            .fields
                            .keys()
                            .any(|name| !fields.contains(name))
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
    if owner.entry_id != checked.contract.owner.entry_id.as_str()
        || owner.entity != checked.contract.owner.entity.as_str()
    {
        return Err("Python compute source catalog/entity ownership mismatch".into());
    }
    Ok(())
}

/// Coverage describes the acquired logical collection; a presentation page can still
/// expose only a prefix. Whole-collection Python must reject that prefix as well.
pub(crate) fn require_complete_collection(
    result: &plasm_runtime::ExecutionResult,
) -> Result<(), String> {
    if result.coverage != ResultCoverage::Complete
        || result.has_more
        || result.paging_handle.is_some()
        || result.pagination_resume.is_some()
    {
        return Err(
            "Python compute needs complete collection coverage without a continuation".into(),
        );
    }
    Ok(())
}

/// Cancellation drops the process session; its supervisor kills and reaps before releasing capacity.
pub(crate) async fn run_worker(
    pool: &crate::python_pool::PythonPool,
    checked: CheckedCompute,
    owner: EntityBinding,
    coverage: ResultCoverage,
    rows: Vec<Value>,
    scope: Option<&crate::operation::ExecutionScope>,
) -> Result<String, String> {
    await_checked(scope, checked.run(pool, &owner, coverage, &rows)).await
}

pub(crate) async fn await_checked<T>(
    scope: Option<&crate::operation::ExecutionScope>,
    future: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
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
            _ = token.cancelled() => return Err("operation cancelled".into()),
            _ = tick.tick() => scope.check()?,
            value = &mut future => { scope.check()?; return value; }
        }
    }
}

pub(crate) fn input_mode(source: &str) -> Result<(bool, bool), String> {
    let ast = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
    let [Stmt::FunctionDef(def)] = ast.suite().as_slice() else {
        return Err("expected compute function".into());
    };
    let ann = def
        .parameters
        .args
        .first()
        .and_then(|p| p.parameter.annotation.as_deref())
        .ok_or("missing compute annotation")?;
    let (value, per_row) = match ann {
        Expr::Subscript(s) if name(&s.value) == Some("list") => (&*s.slice, false),
        _ => (ann, true),
    };
    Ok((name(value) == Some("Row"), per_row))
}

pub(crate) fn validate_input_budget(rows: &[Value]) -> Result<(), String> {
    if rows.len() > MAX_INPUT_ROWS
        || serde_json::to_vec(rows).map_err(|e| e.to_string())?.len() > 1_048_576
    {
        return Err("compute input budget exceeded".into());
    }
    Ok(())
}
