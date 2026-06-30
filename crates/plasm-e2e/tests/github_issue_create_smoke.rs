//! GitHub `issue_create` CML body shape: labels must compile to a JSON string array.

use plasm_compile::{compile_operation, parse_capability_template, CmlEnv};
use plasm_core::loader::load_schema_dir;
use plasm_core::value::Value;
use std::path::PathBuf;

fn github_cgs() -> plasm_core::CGS {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    load_schema_dir(&root.join("../../apis/github")).expect("load apis/github")
}

#[test]
fn issue_create_compiled_body_uses_json_array_for_labels() {
    let cgs = github_cgs();
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("capability templates");
    let cap = cgs.get_capability("issue_create").expect("issue_create");
    let template = parse_capability_template(&cap.mapping.template.0).expect("parse template");

    let mut env = CmlEnv::new();
    env.insert(
        "owner".to_string(),
        Value::String("ryan-s-roberts".to_string()),
    );
    env.insert("repo".to_string(), Value::String("tool-test".to_string()));
    env.insert(
        "title".to_string(),
        Value::String("Document labels".to_string()),
    );
    env.insert("body".to_string(), Value::String("guide body".to_string()));
    env.insert(
        "labels".to_string(),
        Value::Array(vec![
            Value::String("bug".to_string()),
            Value::String("docs".to_string()),
        ]),
    );

    let compiled = compile_operation(&template, &env).expect("compile issue_create");
    let plasm_compile::CompiledOperation::Http(req) = compiled else {
        panic!("expected HTTP operation");
    };
    assert_eq!(req.method_str(), "POST");
    let body = req.body.expect("issue_create POST body");
    let Value::Object(map) = body else {
        panic!("issue_create body must be object, got {body:?}");
    };
    assert_eq!(
        map.get("title"),
        Some(&Value::String("Document labels".to_string()))
    );
    let labels = map.get("labels").expect("labels field");
    let Value::Array(items) = labels else {
        panic!("labels must be JSON array for GitHub API, got {labels:?}");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], Value::String("bug".to_string()));
    assert_eq!(items[1], Value::String("docs".to_string()));
}
