//! Non-executable comparison proofs for compiler-checked teaching examples.
//!
//! Only string equality operands in a simple row-filter lambda are lifted.
//! The field must have a compiler-observed string output contract. Everything
//! else, including thresholds, effects, ordering and provider inputs, remains
//! in the canonical program. The original examples are never rewritten.
use super::teaching::{CheckedTeachingExample, SemanticReference, TeachingProgram};
use plasm_core::value_contract::{ValueContract, ValueShape};
use ruff_python_ast::{
    visitor::{self, Visitor},
    CmpOp, Expr, Stmt,
};
use ruff_text_size::{Ranged, TextRange};
use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq)]
struct Slot {
    field: String,
    contract: ValueContract,
}

#[derive(Debug, PartialEq, Eq)]
struct Signature {
    canonical: String,
    slots: Vec<Slot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiteralParameterizationProof {
    /// Bindings are retained separately; equality relationships are encoded in
    /// the canonical signature and never erased by parameterisation.
    pub(super) original_bindings: Vec<String>,
    pub(super) replacement_bindings: Vec<String>,
    pub(super) original_teaching_tokens: usize,
    pub(super) replacement_teaching_tokens: usize,
    pub(super) same_presentation: bool,
}

struct Extractor<'a> {
    program: &'a TeachingProgram,
    root: &'a QueryRoot,
    bindings: Vec<String>,
    slots: Vec<Slot>,
    edits: Vec<(usize, usize, String)>,
}

/// Prove the filtered row descends from one unambiguous catalogue query.
/// Relation traversals and reassignments remain outside this first proof.
#[derive(Default)]
struct QueryRoot {
    owners: BTreeMap<String, String>,
    assignments: BTreeMap<String, usize>,
    aliases: BTreeMap<String, String>,
    queries: Vec<String>,
    gets: Vec<String>,
    filtered_assignments: BTreeMap<String, TextRange>,
    returns: Vec<ReturnTarget>,
    ambiguous_assignment: bool,
}

enum ReturnTarget {
    Alias(String),
    Filter(TextRange),
    Unsupported,
}

fn return_target(expr: &Expr) -> ReturnTarget {
    match expr {
        Expr::Name(name) => ReturnTarget::Alias(name.id.to_string()),
        Expr::Call(call) => {
            let Expr::Attribute(method) = call.func.as_ref() else {
                return ReturnTarget::Unsupported;
            };
            match method.attr.as_str() {
                "select" | "take" => return_target(&method.value),
                "where" => ReturnTarget::Filter(call.range()),
                _ => ReturnTarget::Unsupported,
            }
        }
        _ => ReturnTarget::Unsupported,
    }
}

impl QueryRoot {
    fn query_owner<'a>(&'a self, expr: &Expr) -> Option<&'a str> {
        let Expr::Call(call) = expr else { return None };
        let Expr::Attribute(method) = call.func.as_ref() else {
            return None;
        };
        if method.attr.as_str() != "query" {
            return None;
        }
        let Expr::Name(owner) = method.value.as_ref() else {
            return None;
        };
        self.owners.get(owner.id.as_str()).map(String::as_str)
    }

    fn is_query_lineage(&self, expr: &Expr, catalogue: &str) -> bool {
        match expr {
            Expr::Name(name) => {
                self.assignments.get(name.id.as_str()) == Some(&1)
                    && self
                        .aliases
                        .get(name.id.as_str())
                        .is_some_and(|owner| owner == catalogue)
            }
            Expr::Call(call) => {
                if self.query_owner(expr) == Some(catalogue) {
                    return true;
                }
                let Expr::Attribute(method) = call.func.as_ref() else {
                    return false;
                };
                method.attr.as_str() == "where" && self.is_query_lineage(&method.value, catalogue)
            }
            _ => false,
        }
    }

    fn is_returned_filter(&self, range: TextRange) -> bool {
        let contains =
            |outer: TextRange| outer.start() <= range.start() && range.end() <= outer.end();
        match self.returns.as_slice() {
            [ReturnTarget::Filter(outer)] => contains(*outer),
            [ReturnTarget::Alias(alias)] => {
                self.assignments.get(alias) == Some(&1)
                    && self
                        .filtered_assignments
                        .get(alias)
                        .is_some_and(|outer| contains(*outer))
            }
            _ => false,
        }
    }
}

impl<'a> Visitor<'a> for QueryRoot {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Assign(assign) if assign.targets.len() == 1 => {
                if let Expr::Name(name) = &assign.targets[0] {
                    *self.assignments.entry(name.id.to_string()).or_default() += 1;
                    if let Some(owner) = self.query_owner(&assign.value).map(str::to_owned) {
                        self.aliases.insert(name.id.to_string(), owner);
                    }
                    if let Expr::Call(call) = assign.value.as_ref() {
                        if matches!(call.func.as_ref(), Expr::Attribute(method) if method.attr.as_str() == "where")
                        {
                            self.filtered_assignments
                                .insert(name.id.to_string(), call.range());
                        }
                    }
                } else {
                    self.ambiguous_assignment = true;
                }
            }
            Stmt::Assign(_)
            | Stmt::AugAssign(_)
            | Stmt::AnnAssign(_)
            | Stmt::For(_)
            | Stmt::While(_) => self.ambiguous_assignment = true,
            Stmt::Return(returned) => self.returns.push(
                returned
                    .value
                    .as_deref()
                    .map_or(ReturnTarget::Unsupported, return_target),
            ),
            _ => {}
        }
        visitor::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Call(call) = expr {
            if let Expr::Attribute(method) = call.func.as_ref() {
                if method.attr.as_str() == "query" {
                    self.queries
                        .push(self.query_owner(expr).unwrap_or("").to_owned());
                } else if method.attr.as_str() == "get" {
                    if let Expr::Name(owner) = method.value.as_ref() {
                        if let Some(catalogue) = self.owners.get(owner.id.as_str()) {
                            self.gets.push(catalogue.clone());
                        }
                    }
                }
            }
        }
        visitor::walk_expr(self, expr);
    }
}

impl Extractor<'_> {
    fn field_contract(&self, field: &str) -> Option<ValueContract> {
        let mut found: Option<ValueContract> = None;
        for output in &self.program.outputs {
            let ValueShape::Record { fields } = &output.row_type.shape else {
                continue;
            };
            let Some(contract) = fields.get(field) else {
                continue;
            };
            if !matches!(
                contract.shape,
                ValueShape::Scalar {
                    field_type: plasm_core::FieldType::String
                }
            ) {
                return None;
            }
            let domain = contract.domain.as_ref()?;
            if self.program.catalog_fingerprints.get(&domain.entry_id) != Some(&domain.catalog_hash)
            {
                return None;
            }
            if found.as_ref().is_some_and(|previous| previous != contract) {
                return None;
            }
            found = Some(contract.clone());
        }
        found
    }

    fn capture_filter(&mut self, call: &ruff_python_ast::ExprCall) {
        let Expr::Attribute(method) = call.func.as_ref() else {
            return;
        };
        if method.attr.as_str() != "where"
            || call.arguments.args.len() != 1
            || !call.arguments.keywords.is_empty()
        {
            return;
        }
        let Some(catalogue) = self.root.queries.first() else {
            return;
        };
        if !self.root.is_query_lineage(&method.value, catalogue)
            || !self.root.is_returned_filter(call.range())
        {
            return;
        }
        let Expr::Lambda(lambda) = &call.arguments.args[0] else {
            return;
        };
        let Some(params) = &lambda.parameters else {
            return;
        };
        if params.args.len() != 1
            || !params.posonlyargs.is_empty()
            || !params.kwonlyargs.is_empty()
            || params.vararg.is_some()
            || params.kwarg.is_some()
            || params.args[0].default.is_some()
        {
            return;
        }
        let Expr::Compare(compare) = lambda.body.as_ref() else {
            return;
        };
        if compare.ops.as_ref() != [CmpOp::Eq] || compare.operands.len() != 2 {
            return;
        }
        let (Expr::Attribute(field), Expr::StringLiteral(literal)) =
            (&compare.operands[0], &compare.operands[1])
        else {
            return;
        };
        let Expr::Name(receiver) = field.value.as_ref() else {
            return;
        };
        if receiver.id.as_str() != params.args[0].name().as_str() {
            return;
        }
        let Some(contract) = self.field_contract(field.attr.as_str()) else {
            return;
        };
        if contract
            .domain
            .as_ref()
            .is_none_or(|domain| &domain.entry_id != catalogue)
        {
            return;
        }
        let value = literal.value.to_str().to_owned();
        let index = match self.bindings.iter().position(|existing| existing == &value) {
            Some(index) => index,
            None => {
                self.bindings.push(value);
                self.bindings.len() - 1
            }
        };
        self.slots.push(Slot {
            field: field.attr.to_string(),
            contract,
        });
        self.edits.push((
            u32::from(literal.range().start()) as usize,
            u32::from(literal.range().end()) as usize,
            format!("\"__plasm_literal_parameter_{index}\""),
        ));
    }

    fn canonical_get_identity(&mut self, call: &ruff_python_ast::ExprCall) {
        let Expr::Attribute(method) = call.func.as_ref() else {
            return;
        };
        let Expr::Name(owner) = method.value.as_ref() else {
            return;
        };
        if method.attr.as_str() != "get"
            || !owner.id.as_str().starts_with("catalog_ref_")
            || !call.arguments.args.is_empty()
        {
            return;
        }
        let Some(first) = call.arguments.keywords.first() else {
            return;
        };
        if first
            .arg
            .as_ref()
            .is_none_or(|arg| arg.as_str() != "identity")
        {
            return;
        }
        // Python admission maps the first positional get argument and identity=
        // to the same identity slot, and rejects duplicate identity operands.
        self.edits.push((
            u32::from(first.range.start()) as usize,
            u32::from(first.value.range().start()) as usize,
            String::new(),
        ));
    }
}

impl<'a> Visitor<'a> for Extractor<'_> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Call(call) = expr {
            self.capture_filter(call);
            self.canonical_get_identity(call);
        }
        visitor::walk_expr(self, expr);
    }
}

fn signature(program: &TeachingProgram) -> Option<(Signature, Vec<String>, String)> {
    // Generated relocation identifiers must never collide with authored names.
    if program.source.contains("catalog_ref_") {
        return None;
    }
    if program
        .occurrences
        .iter()
        .any(|item| matches!(item.reference, SemanticReference::Relation { .. }))
    {
        return None;
    }
    let mut root = QueryRoot::default();
    for item in &program.occurrences {
        if let SemanticReference::Entity { entry_id, .. } = &item.reference {
            use sha2::{Digest, Sha256};
            let encoded = serde_json::to_vec(&item.reference).ok()?;
            root.owners.insert(
                format!("catalog_ref_{}", hex::encode(Sha256::digest(encoded))),
                entry_id.clone(),
            );
        }
    }
    let source = program
        .substitute(|reference| {
            let bytes = serde_json::to_vec(reference)
                .map_err(|_| super::teaching::TeachingError::InvalidLedger)?;
            use sha2::{Digest, Sha256};
            Ok(format!(
                "catalog_ref_{}",
                hex::encode(Sha256::digest(bytes))
            ))
        })
        .ok()?;
    let parsed = ruff_python_parser::parse_module(&source).ok()?;
    for statement in parsed.suite() {
        root.visit_stmt(statement);
    }
    if root.ambiguous_assignment
        || root.queries.len() != 1
        || root.queries[0].is_empty()
        || root
            .gets
            .iter()
            .any(|catalogue| catalogue == &root.queries[0])
    {
        return None;
    }
    let mut visitor = Extractor {
        program,
        root: &root,
        bindings: Vec::new(),
        slots: Vec::new(),
        edits: Vec::new(),
    };
    for statement in parsed.suite() {
        visitor.visit_stmt(statement);
    }
    if visitor.bindings.is_empty() {
        return None;
    }
    visitor.edits.sort_by_key(|edit| edit.0);
    if visitor.edits.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return None;
    }
    let mut normalized = source;
    for (start, end, replacement) in visitor.edits.iter().rev() {
        normalized.replace_range(*start..*end, replacement);
    }
    let parsed = ruff_python_parser::parse_module(&normalized).ok()?;
    let canonical = parsed
        .suite()
        .iter()
        .map(monty::statement_source)
        .collect::<Vec<_>>()
        .join("\n");
    Some((
        Signature {
            canonical,
            slots: visitor.slots,
        },
        visitor.bindings,
        normalized,
    ))
}

pub(super) fn prove(
    original: &CheckedTeachingExample,
    replacement: &CheckedTeachingExample,
) -> Option<LiteralParameterizationProof> {
    if !original.integrity_valid() || !replacement.integrity_valid() {
        return None;
    }
    let a = original.program();
    let b = replacement.program();
    if a.format != b.format
        || a.catalog_fingerprints != b.catalog_fingerprints
        || a.semantic_contracts != b.semantic_contracts
        || a.required_capabilities != b.required_capabilities
        || a.outputs != b.outputs
    {
        return None;
    }
    let (left, original_bindings, original_presentation) = signature(a)?;
    let (right, replacement_bindings, replacement_presentation) = signature(b)?;
    (left == right).then_some(LiteralParameterizationProof {
        original_bindings,
        replacement_bindings,
        original_teaching_tokens: plasm_core::o200k_token_count(&original_presentation),
        replacement_teaching_tokens: plasm_core::o200k_token_count(&replacement_presentation),
        same_presentation: original_presentation == replacement_presentation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_learning::teaching::tests::{session, source};

    async fn checked(
        source: &str,
        session: &crate::execute_session::ExecuteSession,
    ) -> CheckedTeachingExample {
        let bundle = crate::plasm_dag::compile_python_program_checked(session, source).unwrap();
        CheckedTeachingExample::from_execution(session, source, &bundle)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn string_selection_lifts_only_typed_literal_and_preserves_guard() {
        let session = session(&["LangItem"]);
        let template = source(&session).replace(
            ".query().take(1)",
            ".query().where(lambda row: row.title == \"alpha\").select(\"title\")",
        );
        let a = checked(&template, &session).await;
        let b = checked(&template.replace("alpha", "beta"), &session).await;
        let proof = prove(&a, &b).expect("typed equality selection is parameterizable");
        assert_eq!(proof.original_bindings, ["alpha"]);
        assert_eq!(proof.replacement_bindings, ["beta"]);
        assert_eq!(
            proof.original_teaching_tokens, proof.replacement_teaching_tokens,
            "literal length is neutral to teaching quality"
        );
        let changed_guard = checked(&template.replace("==", "!="), &session).await;
        assert!(prove(&a, &changed_guard).is_none());
        let different_output = checked(
            &template.replace("select(\"title\")", "select(\"id\")"),
            &session,
        )
        .await;
        assert!(prove(&a, &different_output).is_none());
    }

    #[tokio::test]
    async fn repeated_parameters_preserve_equality_and_numeric_bounds() {
        let session = session(&["LangItem"]);
        let template = source(&session).replace(".query().take(1)", ".query().where(lambda row: row.title == \"alpha\").where(lambda row: row.title == \"alpha\").select(\"title\").take(1)");
        let first = checked(&template, &session).await;
        let substituted = checked(&template.replace("alpha", "beta"), &session).await;
        assert!(prove(&first, &substituted).is_some());
        let inconsistent = checked(&template.replacen("alpha", "beta", 1), &session).await;
        assert!(prove(&first, &inconsistent).is_none());
        let different_bound = checked(&template.replace("take(1)", "take(2)"), &session).await;
        assert!(prove(&first, &different_bound).is_none());
    }

    #[tokio::test]
    async fn enum_guard_values_are_not_lifted() {
        let session = session(&["LangItem"]);
        let source = source(&session).replace(
            ".query().take(1)",
            ".query().where(lambda row: row.status == \"draft\").select(\"status\")",
        );
        let draft = checked(&source, &session).await;
        let published = checked(&source.replace("draft", "published"), &session).await;
        assert!(prove(&draft, &published).is_none());
    }

    #[tokio::test]
    async fn ambiguous_multiple_query_origins_are_not_parameterized() {
        let session = session(&["LangItem"]);
        let base = source(&session);
        let owner = base
            .lines()
            .last()
            .unwrap()
            .trim()
            .strip_prefix("return ")
            .unwrap()
            .split('.')
            .next()
            .unwrap();
        let filtered = base.replace(
            ".query().take(1)",
            ".query().where(lambda row: row.title == \"alpha\").select(\"title\")",
        );
        let source = filtered.replace(
            &format!("return {owner}.query()"),
            &format!("unused = {owner}.query()\n        return {owner}.query()"),
        );
        let first = checked(&source, &session).await;
        let second = checked(&source.replace("alpha", "beta"), &session).await;
        assert!(prove(&first, &second).is_none());
    }

    #[tokio::test]
    async fn filter_disconnected_from_return_is_not_parameterized() {
        let session = session(&["LangItem"]);
        let base = source(&session);
        let owner = base
            .lines()
            .last()
            .unwrap()
            .trim()
            .strip_prefix("return ")
            .unwrap()
            .split('.')
            .next()
            .unwrap();
        let replacement = format!("rows = {owner}.query()\n        unused = rows.where(lambda row: row.title == \"alpha\")\n        return rows.select(\"title\")");
        let source = base.replace(&format!("return {owner}.query().take(1)"), &replacement);
        let first = checked(&source, &session).await;
        let second = checked(&source.replace("alpha", "beta"), &session).await;
        assert!(prove(&first, &second).is_none());
    }
}
