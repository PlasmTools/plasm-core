//! Scalar syntax must never be silently reinterpreted as literal text.
use super::*;
use crate::{entity_slices_for_render, load_schema_dir, FocusSpec, SymbolMap};
use proptest::prelude::*;

fn schema() -> CGS {
    load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix"),
    )
    .unwrap()
}

#[test]
fn scalar_boundary_rejects_expression_text_in_every_value_lane() {
    let cgs = schema();
    let (slices, _) = entity_slices_for_render(&cgs, FocusSpec::All);
    let symbols: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &slices));
    let layers = [CgsLayer::new("matrix", &cgs)];
    let labels = BTreeSet::from(["source".to_string()]);
    for value in [
        "_.path | split_part(\"/\") | last",
        "missing.field",
        "name + suffix",
        "name * count",
        "name = other",
        "name; other",
        "name[0]",
        "name{field=value}",
        "unknown(value)",
        "name | filter",
        "name \"quoted\"",
        "name => other",
        "1 + 2",
        "true | filter",
        "[name | filter]",
        "v101{nested={title=name | filter}}",
        "v101{nested=[{title=unknown(value)}]}",
    ] {
        for program in [
            format!("LangItem.create(title={value})"),
            format!("LangItem{{owner={value}}}"),
            format!("LangItem({value})"),
            format!("LangItem.create(title=[{value}])"),
        ] {
            assert!(
                parse(&program, &cgs).is_err(),
                "silently accepted: {program}"
            );
            assert!(
                parse_with_cgs_layers_program(
                    &program,
                    &layers,
                    Arc::clone(&symbols),
                    Some(&labels),
                    true
                )
                .is_err(),
                "program mode silently accepted: {program}"
            );
        }
    }
}

proptest! {
    #[test]
    fn scalar_boundary_structural_punctuation_never_becomes_data(
        head in "[a-z]{1,16}", tail in "[a-z]{1,16}",
        op in prop::sample::select(vec![" | ", " + ", " * ", " = ", ";", ".", " => "])
    ) {
        let cgs = schema();
        let text = format!("{head}{op}{tail}");
        for program in [format!("LangItem.create(title={text})"), format!("LangItem{{owner={text}}}")] {
            prop_assert!(parse(&program, &cgs).is_err(), "{}", program);
        }
        // Quoting remains explicit data, independent of expression-looking content.
        let quoted = serde_json::to_string(&text).unwrap();
        let literal = Parser::new(&quoted, &cgs).parse_dotted_call_arg_value_rhs().unwrap();
        prop_assert_eq!(&literal, &Value::String(text.clone()));
        let decoded_literal: Value = serde_json::from_str(&serde_json::to_string(&literal).unwrap()).unwrap();
        prop_assert_eq!(decoded_literal, literal);
        let parsed = parse(&format!("LangItem.create(title={quoted})"), &cgs).unwrap();
        let encoded = serde_json::to_string(&parsed.expr).unwrap();
        let decoded: Expr = serde_json::from_str(&encoded).unwrap();
        prop_assert_eq!(serde_json::to_value(&parsed.expr).unwrap(), serde_json::to_value(decoded).unwrap());
    }
}

#[test]
fn scalar_boundary_preserves_references_and_explicit_literals() {
    let cgs = schema();
    let labels = BTreeSet::from(["source".to_string()]);
    for surface in ["source.title", "_.title"] {
        let mut parser = Parser::new(surface, &cgs);
        parser.program_nodes = Some(&labels);
        parser.for_each_row_context = true;
        let value = parser.parse_dotted_call_arg_value_rhs().unwrap();
        assert!(
            matches!(value, Value::PlasmInputRef(_)),
            "{surface}: {value:?}"
        );
        let decoded: Value = serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(value, decoded);
        assert_eq!(parser.pos, surface.len());
    }
    for (surface, expected) in [
        (r#""_.path | split_part('/')""#, "_.path | split_part('/')"),
        (r"acme\(test\)", "acme(test)"),
        ("Équipe 東京-42", "Équipe 東京-42"),
        (
            "<<BODY\n_.path | split_part('/')\nBODY",
            "_.path | split_part('/')\n",
        ),
    ] {
        let mut parser = Parser::new(surface, &cgs);
        let value = parser.parse_dotted_call_arg_value_rhs().unwrap();
        assert_eq!(value, Value::String(expected.to_owned()), "{surface}");
        assert_eq!(parser.pos, surface.len());
    }
}
