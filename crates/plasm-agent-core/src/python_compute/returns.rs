//! Translate host-owned type references; Monty resolves Python annotations.
use super::*;
use plasm_core::value_contract::ValueContract as Type;
use ruff_python_ast::visitor::{self, Visitor};

/// `Row` is a host-owned placeholder, not a Python type operator. In a
/// multi-input return it refers to the returned record, never the input packet.
pub(super) fn uses_row_placeholder(expr: &Expr) -> bool {
    struct RowUse(bool);
    impl<'a> Visitor<'a> for RowUse {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if matches!(expr, Expr::Name(n) if n.id == "Row") {
                self.0 = true;
            }
            visitor::walk_expr(self, expr);
        }
    }
    let mut found = RowUse(false);
    found.visit_expr(expr);
    found.0
}

pub(super) fn inferred_row_binding(
    output: &Type,
) -> Result<Type, crate::program_rejection::PythonComputeError> {
    use plasm_core::value_contract::ValueShape;
    let record = match &output.shape {
        ValueShape::Array { element } => element.as_ref(),
        _ => output,
    };
    let record = match &record.shape {
        ValueShape::MappingRecord { record } => record.as_ref(),
        _ => record,
    };
    if !matches!(
        record.shape,
        ValueShape::Record { .. } | ValueShape::ObservedRecord { .. }
    ) {
        return Err(crate::program_rejection::PythonComputeError::ReturnRowRequiresRecord);
    }
    let mut row = record.clone();
    // The authored annotation, checked by Monty, owns outer nullability.
    row.nullable = false;
    Ok(row)
}

pub(super) fn resolve(
    expr: &Expr,
    input: &Type,
    domains: &BTreeMap<String, Type>,
    catalogs: &BTreeMap<String, std::sync::Arc<CGS>>,
    cgs: &CGS,
    entry: &str,
    symbols: &dyn SymbolResolve,
    imports: &str,
) -> Result<Type, inference::InferenceError> {
    prepare(expr, input, domains, catalogs, cgs, entry, symbols, imports)?
        .contract()?
        .ok_or(inference::InferenceError::Return(
            inference::ReturnContractError::UnresolvedAnnotation,
        ))
}

pub(super) struct Annotation {
    source: String,
    aliases: BTreeMap<String, Type>,
    imports: String,
}
impl Annotation {
    pub fn contract(&self) -> Result<Option<Type>, inference::InferenceError> {
        inference::annotation_with_imports(&self.source, &self.aliases, &self.imports, None)
    }
    pub fn check_body(
        &self,
        body: &str,
        inputs: &[(&str, &Type)],
        cgs: &CGS,
        catalogs: &BTreeMap<String, std::sync::Arc<CGS>>,
    ) -> Result<(), inference::InferenceError> {
        inference::check_annotated_body(
            body,
            inputs,
            &self.source,
            &self.aliases,
            &self.imports,
            cgs,
            catalogs,
        )
    }
    pub fn check(&self, actual: &Type) -> Result<(), inference::InferenceError> {
        inference::annotation_with_imports(&self.source, &self.aliases, &self.imports, Some(actual))
            .map(|_| ())
    }
}

pub(super) fn prepare(
    expr: &Expr,
    input: &Type,
    domains: &BTreeMap<String, Type>,
    catalogs: &BTreeMap<String, std::sync::Arc<CGS>>,
    cgs: &CGS,
    entry: &str,
    symbols: &dyn SymbolResolve,
    imports: &str,
) -> Result<Annotation, inference::InferenceError> {
    // Catalog references and record-type field selection are Plasm operations.
    // Collect those leaves only; never interpret unions, generics or builtins.
    struct Leaves<'a> {
        values: Vec<&'a Expr>,
    }
    impl<'a> Visitor<'a> for Leaves<'a> {
        fn visit_expr(&mut self, expr: &'a Expr) {
            match expr {
                Expr::Subscript(s) if name(&s.value).is_some_and(is_entity_record_type) => {
                    self.values.push(expr)
                }
                Expr::Attribute(_) => self.values.push(expr),
                _ => visitor::walk_expr(self, expr),
            }
        }
    }
    let mut aliases = domains.clone();
    aliases.insert("Row".into(), input.clone());
    let mut leaves = Leaves { values: vec![] };
    leaves.visit_expr(expr);
    let mut replacements = BTreeMap::new();
    for leaf in leaves.values {
        let contract = match leaf {
            Expr::Subscript(s) => {
                let token = name(&s.slice).ok_or(inference::InferenceError::Return(
                    inference::ReturnContractError::UnresolvedAnnotation,
                ))?;
                let owner = symbols.resolve_session_entity(token)?;
                let (cgs, entry) = if owner.entry_id.as_str() == entry {
                    (cgs, entry)
                } else {
                    (
                        catalogs
                            .get(owner.entry_id.as_str())
                            .map(AsRef::as_ref)
                            .ok_or_else(|| inference::InferenceError::ReturnCatalogMissing {
                                entry_id: owner.entry_id.as_str().to_owned(),
                            })?,
                        owner.entry_id.as_str(),
                    )
                };
                let value = ValueContract::from_cgs(cgs, entry, symbols, token)?;
                Type::record(
                    value
                        .fields
                        .into_iter()
                        .filter(|(_, f)| f.domain.is_some())
                        .map(|(k, f)| (k, f.value_type))
                        .collect(),
                    Default::default(),
                )
            }
            Expr::Attribute(a) => {
                // Qualified Python module types are left intact. Only a host
                // type alias may grant record-field contract projection.
                let root = annotation_root(&a.value);
                if !root.is_some_and(|n| aliases.contains_key(n) || is_entity_record_type(n)) {
                    continue;
                }
                let base = resolve(
                    &a.value, input, domains, catalogs, cgs, entry, symbols, imports,
                )?;
                base.field(a.attr.as_str())?
            }
            _ => unreachable!(),
        };
        let alias = format!("PlasmBoundaryType{}", replacements.len());
        aliases.insert(alias.clone(), contract);
        replacements.insert(monty::expression_source(leaf), alias);
    }
    // Rewrite exact AST leaves, never substrings of Python source.
    struct Rewrite<'a>(&'a BTreeMap<String, String>);
    impl ruff_python_ast::visitor::transformer::Transformer for Rewrite<'_> {
        fn visit_expr(&self, expr: &mut Expr) {
            if let Some(alias) = self.0.get(&monty::expression_source(expr)) {
                *expr = *ruff_python_parser::parse_expression(alias)
                    .expect("generated identifier")
                    .into_syntax()
                    .body;
            } else {
                ruff_python_ast::visitor::transformer::walk_expr(self, expr);
            }
        }
    }
    let mut annotation = expr.clone();
    ruff_python_ast::visitor::transformer::Transformer::visit_expr(
        &Rewrite(&replacements),
        &mut annotation,
    );
    Ok(Annotation {
        source: monty::expression_source(&annotation),
        aliases,
        imports: imports.into(),
    })
}

fn annotation_root(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Attribute(a) => annotation_root(&a.value),
        Expr::Subscript(s) => annotation_root(&s.value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_return_requires_an_inferred_record_contract() {
        assert!(matches!(
            inferred_row_binding(&Type::scalar(plasm_core::FieldType::String)),
            Err(crate::program_rejection::PythonComputeError::ReturnRowRequiresRecord)
        ));
    }
}
