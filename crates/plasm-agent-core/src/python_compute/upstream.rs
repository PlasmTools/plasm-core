//! Python semantics are upstream-owned. This module describes boundary types
//! and capability policy, never expression inference.
use super::*;
use plasm_core::value_contract::{ValueContract as Type, ValueShape};

#[cfg(test)]
pub(super) fn stubs(
    fields: &BTreeMap<String, Type>,
    cgs: &CGS,
) -> Result<String, PythonComputeRejection> {
    stubs_in(fields, cgs, &BTreeMap::new())
}
pub(super) fn stubs_in(
    fields: &BTreeMap<String, Type>,
    cgs: &CGS,
    catalogs: &BTreeMap<String, std::sync::Arc<CGS>>,
) -> Result<String, PythonComputeRejection> {
    let mut declarations = String::from("from typing import Never, Literal, TypeAlias, overload\nPlasmJson: TypeAlias = None | bool | int | float | str | list[PlasmJson] | dict[str, PlasmJson]\nPlasmRef: TypeAlias = bool | int | float | str | dict[str, PlasmRef]\n");
    declarations.push_str(crate::python_money::STUBS);
    let input = Type::record(fields.clone(), Default::default());
    declarations.push_str(crate::python_datetime::PRELUDE);
    declarations.push_str("import datetime as PlasmDatetime\n");
    let ty = render(&input, cgs, catalogs, &mut declarations, 0)?;
    declarations.push_str(&format!("PlasmInput: TypeAlias = {ty}\n"));
    Ok(declarations)
}
pub(super) fn validate_member(name: &str) -> Result<(), PythonComputeRejection> {
    let source = format!("{name} = None");
    let parsed = ruff_python_parser::parse_module(&source)
        .map_err(|_| PythonComputeError::InvalidBoundaryMember { field: name.into() })?;
    if !matches!(parsed.suite().as_slice(), [Stmt::Assign(a)] if matches!(a.targets.as_slice(), [Expr::Name(n)] if n.id.as_str() == name))
        || name.starts_with("__")
    {
        return Err(PythonComputeError::InvalidBoundaryMember { field: name.into() }.into());
    }
    Ok(())
}

/// The same indexed-record contract is used by inference and compute admission.
/// Literal keys retain their field contract; a dynamic string returns the field union.
pub(super) fn record_index_members(
    fields: &[(String, String)],
) -> Result<String, PythonComputeError> {
    if fields.is_empty() {
        return Ok("    def __getitem__(self, key: str) -> Never: ...\n".into());
    }
    let mut out = String::new();
    for (key, ty) in fields {
        out.push_str(&format!(
            "    @overload\n    def __getitem__(self, key: Literal[{}]) -> {ty}: ...\n",
            serde_json::to_string(key).map_err(|error| PythonComputeError::BoundaryKeyEncoding(
                std::sync::Arc::new(error)
            ))?
        ));
    }
    let types = fields.iter().map(|(_, ty)| ty.as_str()).collect::<Vec<_>>();
    // `a | b | ...` parses left-deep. Wide row contracts made the upstream
    // checker exhaust a normal worker stack while inferring an unrelated field.
    let types = balanced_union(&types);
    out.push_str(&format!(
        "    @overload\n    def __getitem__(self, key: str) -> {types}: ...\n"
    ));
    Ok(out)
}

/// Keep generated Python union syntax logarithmic in depth for the upstream checker.
pub(super) fn balanced_union<T: AsRef<str>>(types: &[T]) -> String {
    match types {
        [] => "Never".into(),
        [only] => format!("({})", only.as_ref()),
        _ => {
            let (left, right) = types.split_at(types.len() / 2);
            format!("({}) | ({})", balanced_union(left), balanced_union(right))
        }
    }
}

fn render(
    t: &Type,
    cgs: &CGS,
    catalogs: &BTreeMap<String, std::sync::Arc<CGS>>,
    out: &mut String,
    depth: usize,
) -> Result<String, PythonComputeRejection> {
    if depth >= 64 {
        return Err(PythonComputeError::BoundaryContractDepthExceeded.into());
    }
    let mut ty = match &t.shape {
        ValueShape::Temporal { kind, .. } => format!("PlasmDatetime.{}", kind.python_name()),
        ValueShape::Dictionary { key, value } => format!(
            "dict[{}, {}]",
            render(key, cgs, catalogs, out, depth + 1)?,
            render(value, cgs, catalogs, out, depth + 1)?
        ),
        ValueShape::Never => "Never".into(),
        ValueShape::Null => "None".into(),
        ValueShape::Union { variants } => balanced_union(
            &variants
                .iter()
                .map(|v| render(v, cgs, catalogs, out, depth + 1))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        ValueShape::MappingRecord { record } => {
            use sha2::{Digest, Sha256};
            output_annotation(record, cgs, catalogs, out)?;
            format!(
                "PlasmOutput{:x}",
                Sha256::digest(serde_json::to_vec(&record.shape).map_err(|error| {
                    PythonComputeError::BoundaryFingerprintEncoding(std::sync::Arc::new(error))
                })?,)
            )
        }
        ValueShape::Set { element } => {
            format!("set[{}]", render(element, cgs, catalogs, out, depth + 1)?)
        }
        ValueShape::Array { element } => {
            format!("list[{}]", render(element, cgs, catalogs, out, depth + 1)?)
        }
        ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } => {
            use sha2::{Digest, Sha256};
            let name = format!(
                "PlasmRecord{:x}",
                Sha256::digest(serde_json::to_vec(fields).map_err(|error| {
                    PythonComputeError::BoundaryFingerprintEncoding(std::sync::Arc::new(error))
                })?,)
            );
            if out.contains(&format!("class {name}:")) {
                return Ok(if t.nullable {
                    format!("{name} | None")
                } else {
                    name
                });
            }
            let mut members = String::new();
            let mut indexed = Vec::new();
            for (field, value) in fields {
                validate_member(field)?;
                let ty = render(value, cgs, catalogs, out, depth + 1)?;
                members.push_str(&format!("    {field}: {ty}\n"));
                indexed.push((field.clone(), ty));
            }
            members.push_str(&record_index_members(&indexed)?);
            out.push_str(&format!(
                "class {name}:\n{}",
                if members.is_empty() {
                    "    pass\n"
                } else {
                    &members
                }
            ));
            name
        }
        ValueShape::Scalar { field_type } => match field_type {
            FieldType::Boolean => "bool",
            FieldType::Integer => "int",
            FieldType::Number => "float",
            FieldType::MultiSelect => "list[str]",
            FieldType::Date => return Err(PythonComputeError::BoundaryTemporalShapeRequired.into()),
            FieldType::Money => "dict[str, PlasmJson]",
            FieldType::EntityRef { .. } => "PlasmRef",
            FieldType::Json | FieldType::Blob => "PlasmJson",
            FieldType::Array => return Err(PythonComputeError::BoundaryArrayShapeRequired.into()),
            _ => "str",
        }
        .into(),
    };
    if let Some(domain) = &t.domain {
        let cgs = catalogs
            .get(&domain.entry_id)
            .map(AsRef::as_ref)
            .unwrap_or(cgs);
        if cgs.catalog_cgs_hash_hex() != domain.catalog_hash {
            return Err(PythonComputeError::BoundaryCatalogPinMismatch {
                entry_id: domain.entry_id.clone(),
            }
            .into());
        }
        let value = cgs.values.get(domain.value_ref.as_str()).ok_or_else(|| {
            PythonComputeError::BoundaryValueDomainMissing {
                entry_id: domain.entry_id.clone(),
                value_ref: domain.value_ref.to_string(),
            }
        })?;
        if let Some(tokens) = value.domain.enum_tokens() {
            let literal = format!(
                "Literal[{}]",
                tokens
                    .iter()
                    .map(serde_json::to_string)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| PythonComputeError::BoundaryLiteralEncoding(
                        std::sync::Arc::new(error)
                    ))?
                    .join(", ")
            );
            ty = if matches!(
                t.shape,
                ValueShape::Scalar {
                    field_type: FieldType::MultiSelect
                }
            ) {
                format!("list[{literal}]")
            } else {
                literal
            };
        }
    }
    Ok(if t.nullable {
        format!("{ty} | None")
    } else {
        ty
    })
}

pub(crate) async fn admit_bundle(
    es: &crate::execute_session::ExecuteSession,
    bundle: &crate::plasm_comp_bundle::PlasmCompBundle,
) -> Result<(), crate::compilation_error::CompilationError> {
    let prepared = crate::plan_prepare::prepare_executable_plan_for_session(
        es,
        &bundle.artifact().comp,
        bundle.executable(),
    )
    .map_err(crate::program_diagnostic::ProgramStageError::plan)?;
    let mut definitions = Vec::new();
    let mut plans = vec![prepared.validated.nodes()];
    while let Some(nodes) = plans.pop() {
        for node in nodes {
            for nested in node.nested_plans() {
                plans.push(nested.nodes());
            }
            match node {
                crate::plasm_plan::ValidatedPlanNode::Compute(c)
                    if matches!(
                        c.compute.op,
                        plasm_core::plasm_monad::ComputeOp::Python { .. }
                    ) =>
                {
                    validate_plan_compute(es, c, nodes)
                        .map_err(crate::python_program_diagnostic::admission_error)?;
                    let checked = check_op(es, &c.compute.op)
                        .map_err(crate::python_program_diagnostic::admission_error)?;
                    definitions.push((checked.definition, checked.stubs));
                }
                crate::plasm_plan::ValidatedPlanNode::Compute(c)
                    if !matches!(
                        c.compute.op,
                        plasm_core::ComputeOp::Union { .. }
                            | plasm_core::ComputeOp::MergeBranches { .. }
                            | plasm_core::ComputeOp::Render { .. }
                    ) =>
                {
                    let contract = crate::map_body_schema::row_operation_contract(
                        es,
                        nodes,
                        &c.compute.source,
                    )
                    .map_err(crate::python_program_diagnostic::admission_error)?;
                    let schema = plasm_core::SyntheticResultSchema::for_value(contract)
                        .map_err(crate::python_program_diagnostic::admission_error)?;
                    let contract = schema
                        .row_contract()
                        .map_err(crate::python_program_diagnostic::admission_error)?;
                    let operation = plasm_core::row_plan::plan_node_from_compute(&c.compute.op)
                        .map_err(|error| {
                            crate::program_diagnostic::ProgramStageError::RowCompute { error }
                        })?;
                    plasm_core::row_plan::contracts::output_contract(&contract, &operation)
                        .map_err(crate::python_program_diagnostic::admission_error)?;
                }
                _ => {}
            }
        }
    }
    super::admission::check_definitions(definitions).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn upstream_nullable_and_branch_contracts() {
        let declarations = "class Input:\n    optional: str | None\n    score: int\n";
        for body in [
            "return (row.optional or '').strip()",
            "return str(row.score or '—')",
            "return row.optional.upper() if row.optional is not None else ''",
            "return str(-7)",
            "return str(1.25)",
        ] {
            super::super::admission::check_definition(
                &format!("def render(row: Input) -> str:\n    {body}\n"),
                declarations,
            )
            .unwrap();
        }
        for body in [
            "return row.optional.upper()",
            "return hidden",
            "return row.score",
            "return row.absent",
        ] {
            assert!(
                super::super::admission::check_definition(
                    &format!("def render(row: Input) -> str:\n    {body}\n"),
                    declarations
                )
                .is_err(),
                "accepted {body}"
            );
        }
    }
    #[test]
    fn profile_tracks_vendored_worker_dependency() {
        let cargo = include_str!("../../Cargo.toml");
        let revision = super::LANGUAGE_PROFILE
            .strip_prefix("monty-")
            .unwrap()
            .strip_suffix("-typed-v11-money-v2-branches-v2")
            .unwrap();
        assert!(include_str!("../../../../vendor/README.md")
            .contains(&format!("Initial vendored revision: `{revision}`")));
        for dependency in ["monty-analysis", "monty", "monty-pool", "monty-types"] {
            let declaration = cargo
                .lines()
                .find(|line| line.starts_with(&format!("{dependency} =")))
                .unwrap();
            assert!(
                declaration.contains(&format!(
                    "path = \"../../vendor/monty/crates/{dependency}\""
                )),
                "{dependency} must share the vendored Monty source"
            );
        }
        let worker_build = include_str!("../../../../scripts/ci/build-monty-runtime.sh");
        assert!(worker_build.contains("monty_root=\"${oss_root}/vendor/monty\""));
        assert!(worker_build.contains("--path \"${monty_root}/crates/monty-runtime\""));
    }
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod profile_tests;

/// Keep annotation helpers (Literal, recursive aliases) inside the generated
/// stub module, with an input alias distinct from the output contract.
pub(super) fn input_type(
    t: &Type,
    cgs: &CGS,
    catalogs: &BTreeMap<String, std::sync::Arc<CGS>>,
    declarations: &mut String,
) -> Result<String, PythonComputeRejection> {
    let ty = render(t, cgs, catalogs, declarations, 0)?;
    use sha2::{Digest, Sha256};
    let alias = format!(
        "PlasmArgument{:x}",
        Sha256::digest(serde_json::to_vec(t).map_err(|error| {
            PythonComputeError::BoundaryFingerprintEncoding(std::sync::Arc::new(error))
        })?,)
    );
    declarations.push_str(&format!("{alias}: TypeAlias = {ty}\n"));
    Ok(alias)
}

pub(super) fn output_annotation(
    t: &Type,
    cgs: &CGS,
    catalogs: &BTreeMap<String, std::sync::Arc<CGS>>,
    declarations: &mut String,
) -> Result<String, PythonComputeRejection> {
    fn output(
        t: &Type,
        cgs: &CGS,
        catalogs: &BTreeMap<String, std::sync::Arc<CGS>>,
        out: &mut String,
        depth: usize,
    ) -> Result<String, PythonComputeRejection> {
        if depth >= 64 {
            return Err(PythonComputeError::BoundaryContractDepthExceeded.into());
        }
        let ty = match &t.shape {
            ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } => {
                use sha2::{Digest, Sha256};
                let name = format!(
                    "PlasmOutput{:x}",
                    Sha256::digest(serde_json::to_vec(&t.shape).map_err(|error| {
                        PythonComputeError::BoundaryFingerprintEncoding(std::sync::Arc::new(error))
                    })?,)
                );
                let class = render(t, cgs, catalogs, out, depth)?;
                if !out.contains(&format!("{name} = TypedDict")) {
                    let mut members = Vec::new();
                    for (key, value) in fields {
                        let mut ty = output(value, cgs, catalogs, out, depth + 1)?;
                        if matches!(&t.shape, ValueShape::ObservedRecord { optional_fields, .. } if optional_fields.contains(key))
                        {
                            ty = format!("NotRequired[{ty}]");
                        }
                        members.push(format!(
                            "{}: {ty}",
                            serde_json::to_string(key).map_err(|error| {
                                PythonComputeError::BoundaryKeyEncoding(std::sync::Arc::new(error))
                            })?
                        ));
                    }
                    out.push_str(&format!(
                        "{name} = TypedDict(\"{name}\", {{{}}})\n",
                        members.join(", ")
                    ));
                }
                format!("{class} | {name}")
            }
            ValueShape::Dictionary { key, value } => format!(
                "dict[{}, {}]",
                render(key, cgs, catalogs, out, depth + 1)?,
                output(value, cgs, catalogs, out, depth + 1)?
            ),
            ValueShape::Array { element } => {
                let element = output(element, cgs, catalogs, out, depth + 1)?;
                format!(
                    "{} | list[{element}] | tuple[{element}, ...]",
                    render(t, cgs, catalogs, out, depth)?,
                )
            }
            ValueShape::Union { variants } => balanced_union(
                &variants
                    .iter()
                    .map(|v| output(v, cgs, catalogs, out, depth + 1))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            _ => return render(t, cgs, catalogs, out, depth),
        };
        Ok(if t.nullable {
            format!("{ty} | None")
        } else {
            ty
        })
    }
    declarations.push_str("from typing import TypedDict, NotRequired\n");
    output(t, cgs, catalogs, declarations, 0)
}

pub(super) fn output_type(
    t: &Type,
    cgs: &CGS,
    catalogs: &BTreeMap<String, std::sync::Arc<CGS>>,
    declarations: &mut String,
) -> Result<String, PythonComputeRejection> {
    let ty = output_annotation(t, cgs, catalogs, declarations)?;
    declarations.push_str(&format!("PlasmOutput: TypeAlias = {ty}\n"));
    Ok("PlasmOutput".into())
}

pub(super) fn domain_aliases(
    domains: &BTreeMap<String, Type>,
    cgs: &CGS,
    catalogs: &BTreeMap<String, std::sync::Arc<CGS>>,
    declarations: &mut String,
) -> Result<(), PythonComputeRejection> {
    for (symbol, contract) in domains {
        let ty = render(contract, cgs, catalogs, declarations, 0)?;
        declarations.push_str(&format!("{symbol}: TypeAlias = {ty}\n"));
    }
    Ok(())
}
