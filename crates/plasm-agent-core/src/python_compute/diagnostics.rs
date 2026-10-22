//! Agent-facing names for checker implementation types. This never changes admission.
use ruff_python_ast::{Expr, Stmt};
use std::collections::BTreeMap;

/// Recover public shapes from the exact declarations supplied to the checker.
/// Keep this separate from the original diagnostics retained for inspection.
pub(super) fn labels(stubs: &str) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    if let Ok(module) = ruff_python_parser::parse_module(stubs) {
        for statement in module.suite() {
            match statement {
                Stmt::ClassDef(class) => {
                    let fields = class
                        .body
                        .iter()
                        .filter_map(|statement| {
                            match statement {
                                Stmt::AnnAssign(field) => {
                                    let Expr::Name(name) = field.target.as_ref() else { return None };
                                    Some(format!("{}: {}", name.id, monty::expression_source(&field.annotation)))
                                }
                                Stmt::FunctionDef(field) if field.decorator_list.iter().any(|d| matches!(&d.expression, Expr::Name(n) if n.id == "property")) => {
                                    field.returns.as_ref().map(|ty| format!("{}: {}", field.name, monty::expression_source(ty)))
                                }
                                _ => None,
                            }
                        })
                        .collect::<Vec<_>>();
                    if !fields.is_empty()
                        || class.name.starts_with("PlasmRecord")
                        || class.name.starts_with("PlasmAnalysisType")
                    {
                        labels.insert(
                            class.name.to_string(),
                            format!("record{{{}}}", fields.join(", ")),
                        );
                    }
                }
                Stmt::AnnAssign(alias) => {
                    if let (Expr::Name(name), Some(value)) = (alias.target.as_ref(), &alias.value) {
                        labels.insert(name.id.to_string(), monty::expression_source(value));
                    }
                }
                Stmt::Assign(assignment) => {
                    let ([Expr::Name(name)], Expr::Call(call)) =
                        (assignment.targets.as_slice(), assignment.value.as_ref())
                    else {
                        continue;
                    };
                    if matches!(call.func.as_ref(), Expr::Name(n) if n.id == "NewType") {
                        if let Some(base) = call.arguments.args.get(1) {
                            labels.insert(name.id.to_string(), monty::expression_source(base));
                        }
                        continue;
                    }
                    if !matches!(call.func.as_ref(), Expr::Name(n) if n.id == "TypedDict") {
                        continue;
                    }
                    let Some(Expr::Dict(dict)) = call.arguments.args.get(1) else {
                        continue;
                    };
                    let fields = dict
                        .items
                        .iter()
                        .filter_map(|item| {
                            let Some(Expr::StringLiteral(key)) = &item.key else {
                                return None;
                            };
                            Some(format!(
                                "{}: {}",
                                key.value.to_str(),
                                monty::expression_source(&item.value)
                            ))
                        })
                        .collect::<Vec<_>>();
                    labels.insert(
                        name.id.to_string(),
                        format!("mapping{{{}}}", fields.join(", ")),
                    );
                }
                _ => {}
            }
        }
    }
    labels.extend([
        ("PlasmJson".into(), "JSON value".into()),
        ("PlasmRef".into(), "entity reference".into()),
        ("PlasmDatetime".into(), "datetime".into()),
        ("__plasm_expected".into(), "declared input contract".into()),
        ("__plasm_return".into(), "compute return".into()),
        ("__plasm_annotation".into(), "declared annotation".into()),
        ("__plasm_expression".into(), "compute expression".into()),
        ("__plasm_input".into(), "input".into()),
        ("_PlasmSelected".into(), "element type".into()),
        ("_PlasmDefault".into(), "default value type".into()),
    ]);
    labels
}

pub(super) fn semantic_text(text: &str, labels: &BTreeMap<String, String>) -> String {
    const MAX_CHARS: usize = 1600;
    const MAX_DEPTH: usize = 8;
    fn render(
        text: &str,
        labels: &BTreeMap<String, String>,
        active: &mut Vec<String>,
        out: &mut String,
        remaining: &mut usize,
    ) {
        fn append(out: &mut String, remaining: &mut usize, text: &str) {
            for ch in text.chars().take(*remaining) {
                out.push(ch);
                *remaining -= 1;
            }
        }
        for part in text.split_inclusive(|c: char| !c.is_alphanumeric() && c != '_') {
            if *remaining == 0 {
                break;
            }
            let end = part
                .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .len();
            let (word, punctuation) = part.split_at(end);
            if let Some(label) = labels.get(word) {
                if active.iter().any(|name| name == word) {
                    append(out, remaining, "recursive value");
                } else if active.len() >= MAX_DEPTH {
                    append(out, remaining, "nested value (type detail omitted)");
                } else if active.is_empty() {
                    // Budget shapes separately so expected cannot hide found.
                    let mut expanded = String::new();
                    let mut allowance = 480;
                    active.push(word.into());
                    render(label, labels, active, &mut expanded, &mut allowance);
                    active.pop();
                    append(out, remaining, &expanded);
                    if allowance == 0 {
                        append(out, remaining, " … (type detail omitted)");
                    }
                } else {
                    active.push(word.into());
                    render(label, labels, active, out, remaining);
                    active.pop();
                }
            } else {
                append(out, remaining, word);
            }
            append(out, remaining, punctuation);
        }
    }
    let mut out = String::new();
    let mut remaining = MAX_CHARS;
    render(text, labels, &mut Vec::new(), &mut out, &mut remaining);
    if remaining == 0 {
        out.push_str(" … (remaining type detail omitted)");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_type_families_render_structural_contracts() {
        let stubs = "from typing import TypeAlias, TypedDict, NewType, NotRequired\nPlasmAnalysisType7 = NewType('PlasmAnalysisType7', int)\nclass PlasmRecordabc:\n    id: PlasmAnalysisType7\n    title: str | None\nPlasmOutputabc = TypedDict('PlasmOutputabc', {'id': int, 'title': NotRequired[str | None]})\nPlasmMappingabc = TypedDict('PlasmMappingabc', {'item': PlasmRecordabc})\nPlasmArgumentabc: TypeAlias = list[PlasmRecordabc]\n";
        let names = labels(stubs);
        for (input, expected) in [
            (
                "PlasmArgumentabc",
                "list[record{id: int, title: str | None}]",
            ),
            (
                "PlasmMappingabc",
                "mapping{item: record{id: int, title: str | None}}",
            ),
            (
                "PlasmOutputabc",
                "mapping{id: int, title: NotRequired[str | None]}",
            ),
            ("PlasmAnalysisType7", "int"),
        ] {
            assert_eq!(semantic_text(input, &names), expected);
        }
        assert_eq!(
            semantic_text("PlasmRecordabc_user", &names),
            "PlasmRecordabc_user"
        );
    }

    #[test]
    fn cyclic_aliases_do_not_recurse_forever() {
        let names = BTreeMap::from([("PlasmInput".into(), "list[PlasmInput]".into())]);
        assert_eq!(semantic_text("PlasmInput", &names), "list[recursive value]");
    }

    #[test]
    fn shared_deep_and_wide_shapes_have_bounded_presentation() {
        let mut names = BTreeMap::new();
        for i in 0..100 {
            names.insert(
                format!("PlasmRecord{i}"),
                format!(
                    "record{{left: PlasmRecord{}, right: PlasmRecord{}}}",
                    i + 1,
                    i + 1
                ),
            );
        }
        names.insert("PlasmRecord100".into(), "record{id: int}".into());
        for text in ["PlasmRecord0".to_owned(), "PlasmRecord99 ".repeat(1000)] {
            let rendered = semantic_text(&text, &names);
            assert!(rendered.chars().count() < 1700);
            assert!(rendered.contains("detail omitted"));
            assert!(!rendered.contains("PlasmRecord"));
        }
        let stubs = "class PlasmRecordEmpty:\n    pass\nclass PlasmAnalysisType0:\n    @property\n    def id(self) -> int: ...\n";
        assert_eq!(
            semantic_text("PlasmRecordEmpty", &labels(stubs)),
            "record{}"
        );
        assert_eq!(
            semantic_text("PlasmAnalysisType0", &labels(stubs)),
            "record{id: int}"
        );
        names.insert(
            "Expected".into(),
            format!("record{{{}}}", "field: str, ".repeat(1000)),
        );
        names.insert("Actual".into(), "record{id: int}".into());
        let rendered = semantic_text("Expected Expected; found Actual", &names);
        assert!(rendered.contains("found record{id: int}"), "{rendered}");
    }

    #[test]
    fn actual_checker_admission_reports_shapes_and_repair_without_internal_names() {
        let stubs = "from typing import TypedDict\nclass PlasmRecordabc:\n    id: int\nPlasmOutputabc = TypedDict('PlasmOutputabc', {'id': str})\n";
        for source in [
            "def f(value: PlasmRecordabc) -> PlasmOutputabc:\n    return value\n",
            "def target(value: PlasmOutputabc) -> None:\n    pass\ndef f(value: PlasmRecordabc) -> None:\n    target(value)\n",
            "def f(value: PlasmRecordabc) -> str:\n    return value.missing\n",
        ] {
            let error = crate::python_compute::admission::check_definition(source, stubs).unwrap_err();
            let rendered = error.to_string();
            assert!(!rendered.contains("PlasmRecord"), "{rendered}");
            assert!(!rendered.contains("PlasmOutput"), "{rendered}");
            assert!(rendered.contains("help:"), "{rendered}");
        }
    }
}

pub(super) fn help(code: &str) -> Option<&'static str> {
    match code {
        "invalid-return-type" => Some("Return the declared field shape and value types, or correct the return annotation; keep optional fields distinct from null."),
        "invalid-argument-type" => Some("Match the expected input shape and value types; a single record is not a list of records."),
        "unresolved-attribute" => Some("Use a field exposed by this value's contract; narrow nullable or union values before reading variant fields."),
        "unresolved-reference" => Some("Define this name in the compute scope or pass it as an explicit compute input."),
        _ => None,
    }
}
