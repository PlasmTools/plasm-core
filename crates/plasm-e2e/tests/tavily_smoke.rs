//! Tavily catalog smoke: validate mappings and research_create JSON body shape.

use plasm_compile::{compile_operation, parse_capability_template, CmlEnv};
use plasm_core::loader::load_schema_dir;
use plasm_core::value::Value;
use std::path::PathBuf;

fn tavily_cgs() -> plasm_core::CGS {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("../../apis/tavily");
    load_schema_dir(&dir).expect("load apis/tavily")
}

#[test]
fn tavily_mappings_validate() {
    let cgs = tavily_cgs();
    cgs.validate().expect("CGS validate");
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("capability templates");
}

#[test]
fn research_create_compiled_body_is_json_object_with_input_key() {
    let cgs = tavily_cgs();
    let cap = cgs
        .get_capability("research_create")
        .expect("research_create capability");
    let template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template.0)
            .expect("parse template");

    let aggregate = Value::Object(
        [
            (
                "input".to_string(),
                Value::String("quantum error correction".to_string()),
            ),
            ("model".to_string(), Value::String("auto".to_string())),
        ]
        .into_iter()
        .collect(),
    );

    let mut env = CmlEnv::new();
    env.insert("input".to_string(), aggregate.clone());
    if let Value::Object(ref map) = aggregate {
        for (k, v) in map {
            env.insert(k.clone(), v.clone());
        }
    }

    let compiled = compile_operation(&template, &env).expect("compile research_create");
    let plasm_compile::CompiledOperation::Http(req) = compiled else {
        panic!("expected HTTP operation");
    };
    let body = req.body.expect("POST /research must have a body");
    let Value::Object(map) = body else {
        panic!("research_create body must be a JSON object, got {body:?}");
    };
    assert!(
        map.get("input")
            .is_some_and(|v| matches!(v, Value::String(_))),
        "body must include string input field: {map:?}"
    );
    assert_eq!(map.get("model"), Some(&Value::String("auto".to_string())));
}
