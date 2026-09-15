#[cfg(not(test))]
use std::sync::atomic::{AtomicBool, Ordering};

use crate::constants::{
    DEFAULT_USE_EXAMPLES, OBJECT_CIRCULAR_REFS_MAX_DEPTH, PERCENT_CHANCE_FOR_NULLABLE_TO_BE_NULL,
};
use crate::primitive_generator::{primitive_fallback, random_word};
use crate::spec_parser::{flatten_schema, flatten_schema_forced};
use rand::RngExt;
use serde_json::Value as JsonValue;
use std::collections::{HashMap, VecDeque};
use yaml_serde::Value as YamlValue;

#[cfg(not(test))]
static USE_EXAMPLES: AtomicBool = AtomicBool::new(DEFAULT_USE_EXAMPLES);

#[cfg(test)]
thread_local! {
    static USE_EXAMPLES_LOCAL: std::cell::Cell<bool> =
        const { std::cell::Cell::new(DEFAULT_USE_EXAMPLES) };
}

pub fn set_use_examples(val: bool) {
    #[cfg(not(test))]
    USE_EXAMPLES.store(val, Ordering::Relaxed);
    #[cfg(test)]
    USE_EXAMPLES_LOCAL.set(val);
}

pub fn use_examples() -> bool {
    #[cfg(not(test))]
    {
        USE_EXAMPLES.load(Ordering::Relaxed)
    }
    #[cfg(test)]
    {
        USE_EXAMPLES_LOCAL.get()
    }
}

#[cfg(test)]
pub(crate) struct UseExamplesGuard;

#[cfg(test)]
impl Drop for UseExamplesGuard {
    fn drop(&mut self) {
        set_use_examples(DEFAULT_USE_EXAMPLES);
    }
}

enum Frame {
    Eval {
        schema: YamlValue,
        forced: Option<String>,
    },
    BuildObject {
        pending_keys: VecDeque<String>,
    },
    BuildArray,
    ExitRef {
        ref_path: String,
    },
}

pub fn generate(schema: &YamlValue, root: &YamlValue, forced_variant: Option<&str>) -> JsonValue {
    let mut frame_stack: Vec<Frame> = vec![Frame::Eval {
        schema: schema.clone(),
        forced: forced_variant.map(String::from),
    }];
    let mut value_stack: Vec<JsonValue> = Vec::new();
    let mut visiting_refs: HashMap<String, usize> = HashMap::new();

    while let Some(frame) = frame_stack.pop() {
        match frame {
            Frame::Eval { schema, forced } => process_eval(
                schema,
                forced,
                root,
                &mut frame_stack,
                &mut value_stack,
                &mut visiting_refs,
            ),
            Frame::BuildObject { pending_keys } => collect_object(pending_keys, &mut value_stack),
            Frame::BuildArray => collect_array(&mut value_stack),
            Frame::ExitRef { ref_path } => exit_ref(ref_path, &mut visiting_refs),
        }
    }

    value_stack.pop().unwrap_or(JsonValue::Null)
}

fn process_eval(
    schema: YamlValue,
    forced: Option<String>,
    root: &YamlValue,
    frame_stack: &mut Vec<Frame>,
    value_stack: &mut Vec<JsonValue>,
    visiting_refs: &mut HashMap<String, usize>,
) {
    if let Some(ref_path) = ref_path_of(&schema) {
        let count = visiting_refs.entry(ref_path.clone()).or_insert(0);
        if *count > OBJECT_CIRCULAR_REFS_MAX_DEPTH {
            value_stack.push(JsonValue::Null);
            return;
        }
        *count += 1;
        frame_stack.push(Frame::ExitRef { ref_path });
    }
    let flat = flatten(&schema, root, forced.as_deref());
    if let Some(value) = override_value(&flat) {
        value_stack.push(value);
        return;
    }
    schedule(flat, forced, frame_stack, value_stack);
}

// When `$ref` is absent, `flatten` processes the original schema regardless,
// so an empty ref path and a missing ref path produce the same output.
#[cfg_attr(test, mutants::skip)]
fn ref_path_of(schema: &YamlValue) -> Option<String> {
    schema.get("$ref")?.as_str().map(String::from)
}

fn exit_ref(ref_path: String, visiting_refs: &mut HashMap<String, usize>) {
    if let Some(count) = visiting_refs.get_mut(&ref_path) {
        *count -= 1;
    }
}

fn flatten(schema: &YamlValue, root: &YamlValue, forced: Option<&str>) -> YamlValue {
    match forced {
        Some(v) => flatten_schema_forced(schema, root, v),
        None => flatten_schema(schema, root),
    }
}

// The threshold for nullable probability is arbitrary. Any nearby
// threshold produces indistinguishable test outcomes because the RNG is unseeded.
#[cfg_attr(test, mutants::skip)]
fn override_value(flat: &YamlValue) -> Option<JsonValue> {
    if use_examples() {
        if let Some(example) = flat.get("example") {
            return Some(yaml_to_json(example));
        }
        if let Some(default) = flat.get("default") {
            return Some(yaml_to_json(default));
        }
    }
    if let Some(enum_seq) = flat.get("enum").and_then(|v| v.as_sequence())
        && !enum_seq.is_empty()
    {
        let idx = rand::rng().random_range(0..enum_seq.len());
        return Some(yaml_to_json(&enum_seq[idx]));
    }
    if flat
        .get("nullable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        && rand::rng().random_range(0..100) < PERCENT_CHANCE_FOR_NULLABLE_TO_BE_NULL
    {
        return Some(JsonValue::Null);
    }
    None
}

fn schedule(
    flat: YamlValue,
    forced: Option<String>,
    frame_stack: &mut Vec<Frame>,
    value_stack: &mut Vec<JsonValue>,
) {
    match flat.get("type").and_then(|v| v.as_str()).unwrap_or("") {
        "object" => schedule_object_children(flat, forced, frame_stack),
        "array" => schedule_array_child(flat, forced, frame_stack, value_stack),
        t => value_stack.push(primitive_fallback(&flat, t)),
    }
}

fn schedule_object_children(flat: YamlValue, forced: Option<String>, frame_stack: &mut Vec<Frame>) {
    let mut pending_keys: VecDeque<String> = VecDeque::new();
    let mut eval_frames: Vec<Frame> = Vec::new();

    if let Some(props) = flat.get("properties").and_then(|v| v.as_mapping()) {
        for (k, v) in props {
            if let Some(key) = k.as_str() {
                let is_write_only = v
                    .get("writeOnly")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false);
                if !is_write_only {
                    pending_keys.push_back(key.to_string());
                    eval_frames.push(Frame::Eval {
                        schema: v.clone(),
                        forced: forced.clone(),
                    });
                }
            }
        }
    }

    if let Some(add_props) = flat.get("additionalProperties")
        && add_props.as_bool() != Some(false)
        && add_props.is_mapping()
    {
        let mut rng = rand::rng();
        let count = rng.random_range(1..=2usize);
        for _ in 0..count {
            let key = random_word(&mut rng).to_string();
            pending_keys.push_back(key);
            eval_frames.push(Frame::Eval {
                schema: add_props.clone(),
                forced: forced.clone(),
            });
        }
    }

    // Keys are queued front-to-back; eval frames are stacked in the same order so the frame
    // stack (LIFO) evaluates them back-to-front, landing values on value_stack in reverse.
    // collect_object drains keys front-to-back and values top-to-bottom -- the two reversals
    // cancel, pairing each key with its value correctly.
    frame_stack.push(Frame::BuildObject { pending_keys });
    frame_stack.extend(eval_frames);
}

fn schedule_array_child(
    flat: YamlValue,
    forced: Option<String>,
    frame_stack: &mut Vec<Frame>,
    value_stack: &mut Vec<JsonValue>,
) {
    match flat.get("items") {
        Some(items_schema) => {
            frame_stack.push(Frame::BuildArray);
            frame_stack.push(Frame::Eval {
                schema: items_schema.clone(),
                forced,
            });
        }
        None => value_stack.push(JsonValue::Array(vec![])),
    }
}

fn collect_object(mut pending_keys: VecDeque<String>, value_stack: &mut Vec<JsonValue>) {
    let mut map = serde_json::Map::new();
    while let Some(key) = pending_keys.pop_front() {
        let val = value_stack
            .pop()
            .expect("value stack should have a value for each pending object property");
        map.insert(key, val);
    }
    value_stack.push(JsonValue::Object(map));
}

fn collect_array(value_stack: &mut Vec<JsonValue>) {
    let item = value_stack
        .pop()
        .expect("value stack should have a value for array item");
    value_stack.push(JsonValue::Array(vec![item]));
}

fn yaml_to_json(v: &YamlValue) -> JsonValue {
    serde_json::to_value(v).unwrap_or(JsonValue::Null)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{UseExamplesGuard, generate, set_use_examples};

    fn yaml(s: &str) -> yaml_serde::Value {
        yaml_serde::from_str(s).unwrap()
    }

    // --- use_examples global ---

    #[test]
    fn generate_does_not_use_example_when_global_set() {
        let _guard = UseExamplesGuard;
        set_use_examples(false);
        let root = yaml("{}");
        let result = generate(&yaml("type: string\nexample: hello"), &root, None);
        assert!(result.is_string());
        assert_ne!(result, json!("hello"));
    }

    // --- example takes priority ---

    #[test]
    fn generate_returns_top_level_example() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("{}");
        let schema = yaml("type: string\nexample: hello");
        assert_eq!(generate(&schema, &root, None), json!("hello"));
    }

    // --- object generation ---

    #[test]
    fn generate_object_includes_all_properties() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("{}");
        let schema = yaml(
            "type: object\n\
             properties:\n\
             \x20 name:\n\
             \x20   type: string\n\
             \x20   example: Alice\n\
             \x20 age:\n\
             \x20   type: integer\n\
             \x20   example: 30",
        );
        let result = generate(&schema, &root, None);
        assert_eq!(result["name"], json!("Alice"));
        assert_eq!(result["age"], json!(30));
    }

    #[test]
    fn generate_object_with_no_properties_returns_empty_object() {
        let root = yaml("{}");
        let schema = yaml("type: object");
        assert_eq!(generate(&schema, &root, None), json!({}));
    }

    // --- array generation ---

    #[test]
    fn generate_array_returns_single_item_from_items_schema() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("{}");
        let schema = yaml("type: array\nitems:\n  type: string\n  example: item");
        let result = generate(&schema, &root, None);
        assert!(result.is_array());
        assert_eq!(result.as_array().unwrap().len(), 1);
        assert_eq!(result[0], json!("item"));
    }

    #[test]
    fn generate_array_with_example_returns_example() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("{}");
        let schema = yaml("type: array\nexample: [a, b]\nitems:\n  type: string");
        assert_eq!(generate(&schema, &root, None), json!(["a", "b"]));
    }

    #[test]
    fn generate_array_without_items_returns_empty_array() {
        let root = yaml("{}");
        let schema = yaml("type: array");
        assert_eq!(generate(&schema, &root, None), json!([]));
    }

    // --- primitive defaults ---

    #[test]
    fn generate_string_without_example_returns_a_non_empty_string() {
        let root = yaml("{}");
        let result = generate(&yaml("type: string"), &root, None);
        assert!(result.as_str().map(|s| !s.is_empty()).unwrap_or(false));
    }

    #[test]
    fn generate_string_without_example_is_a_multi_word_phrase() {
        let root = yaml("{}");
        let result = generate(&yaml("type: string"), &root, None);
        let s = result.as_str().expect("generated value should be a string");
        assert!(s.contains(' '), "expected a multi-word phrase, got {s:?}");
    }

    #[test]
    fn generate_integer_without_example_returns_a_positive_integer() {
        let root = yaml("{}");
        let result = generate(&yaml("type: integer"), &root, None);
        assert!(result.as_i64().map(|n| n > 0).unwrap_or(false));
    }

    #[test]
    fn generate_number_without_example_returns_a_positive_number() {
        let root = yaml("{}");
        let result = generate(&yaml("type: number"), &root, None);
        assert!(result.as_f64().map(|n| n > 0.0).unwrap_or(false));
    }

    #[test]
    fn generate_boolean_without_example_returns_a_boolean() {
        let root = yaml("{}");
        let result = generate(&yaml("type: boolean"), &root, None);
        assert!(result.is_boolean());
    }

    #[test]
    fn generate_unknown_type_returns_null() {
        let root = yaml("{}");
        assert_eq!(generate(&yaml("type: unknown"), &root, None), json!(null));
    }

    // --- enums ---

    #[test]
    fn generate_string_enum_returns_one_of_the_enum_values() {
        let root = yaml("{}");
        let schema = yaml("type: string\nenum: [foo, bar, baz]");
        let result = generate(&schema, &root, None);
        assert!(["foo", "bar", "baz"].contains(&result.as_str().unwrap()));
    }

    #[test]
    fn generate_integer_enum_returns_one_of_the_enum_values() {
        let root = yaml("{}");
        let schema = yaml("type: integer\nenum: [1, 2, 3]");
        let result = generate(&schema, &root, None);
        assert!([1, 2, 3].contains(&(result.as_i64().unwrap() as i32)));
    }

    #[test]
    fn generate_single_value_enum_always_returns_that_value() {
        let root = yaml("{}");
        let schema = yaml("type: string\nenum: [only]");
        assert_eq!(generate(&schema, &root, None), json!("only"));
    }

    // --- formats ---

    fn assert_matches(value: &serde_json::Value, pattern: &str) {
        let s = value.as_str().expect("generated value should be a string");
        let re = regex::Regex::new(pattern).unwrap();
        assert!(
            re.is_match(s),
            "value {s:?} did not match pattern {pattern:?}"
        );
    }

    #[test]
    fn generate_date_time_format_matches_iso8601() {
        let root = yaml("{}");
        assert_matches(
            &generate(&yaml("type: string\nformat: date-time"), &root, None),
            r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}",
        );
    }

    #[test]
    fn generate_date_format_matches_iso8601_date() {
        let root = yaml("{}");
        assert_matches(
            &generate(&yaml("type: string\nformat: date"), &root, None),
            r"^\d{4}-\d{2}-\d{2}$",
        );
    }

    #[test]
    fn generate_time_format_matches_hhmmss() {
        let root = yaml("{}");
        assert_matches(
            &generate(&yaml("type: string\nformat: time"), &root, None),
            r"^\d{2}:\d{2}:\d{2}",
        );
    }

    #[test]
    fn generate_uuid_format_matches_uuid_pattern() {
        let root = yaml("{}");
        assert_matches(
            &generate(&yaml("type: string\nformat: uuid"), &root, None),
            r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
        );
    }

    #[test]
    fn generate_email_format_matches_email_pattern() {
        let root = yaml("{}");
        assert_matches(
            &generate(&yaml("type: string\nformat: email"), &root, None),
            r"^[^@\s]+@[^@\s]+\.[^@\s]+$",
        );
    }

    #[test]
    fn generate_uri_format_matches_http_url() {
        let root = yaml("{}");
        assert_matches(
            &generate(&yaml("type: string\nformat: uri"), &root, None),
            r"^https?://",
        );
    }

    #[test]
    fn generate_hostname_format_matches_hostname_pattern() {
        let root = yaml("{}");
        assert_matches(
            &generate(&yaml("type: string\nformat: hostname"), &root, None),
            r"^[a-zA-Z0-9]([a-zA-Z0-9\-\.]*[a-zA-Z0-9])?$",
        );
    }

    #[test]
    fn generate_ipv4_format_matches_dotted_quad() {
        let root = yaml("{}");
        assert_matches(
            &generate(&yaml("type: string\nformat: ipv4"), &root, None),
            r"^\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$",
        );
    }

    #[test]
    fn generate_ipv6_format_matches_ipv6_pattern() {
        let root = yaml("{}");
        assert_matches(
            &generate(&yaml("type: string\nformat: ipv6"), &root, None),
            r"^[0-9a-fA-F:]+$",
        );
    }

    #[test]
    fn generate_byte_format_matches_base64() {
        let root = yaml("{}");
        assert_matches(
            &generate(&yaml("type: string\nformat: byte"), &root, None),
            r"^[A-Za-z0-9+/]+=*$",
        );
    }

    #[test]
    fn generate_unknown_format_returns_empty_string() {
        let root = yaml("{}");
        let result = generate(&yaml("type: string\nformat: unknown-format"), &root, None);
        assert_eq!(result, json!(""));
    }

    #[test]
    fn generate_password_format_returns_non_empty_string() {
        let root = yaml("{}");
        let result = generate(&yaml("type: string\nformat: password"), &root, None);
        assert!(result.as_str().map(|s| !s.is_empty()).unwrap_or(false));
    }

    // --- $ref and allOf ---

    #[test]
    fn generate_follows_ref() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("Name:\n  type: string\n  example: Alice");
        let schema = yaml("$ref: '#/Name'");
        assert_eq!(generate(&schema, &root, None), json!("Alice"));
    }

    #[test]
    fn generate_merges_all_of() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("{}");
        let schema = yaml(
            "allOf:\n\
             \x20 - type: object\n\
             \x20   properties:\n\
             \x20     a:\n\
             \x20       type: string\n\
             \x20       example: alpha\n\
             \x20 - type: object\n\
             \x20   properties:\n\
             \x20     b:\n\
             \x20       type: integer\n\
             \x20       example: 42",
        );
        let result = generate(&schema, &root, None);
        assert_eq!(result["a"], json!("alpha"));
        assert_eq!(result["b"], json!(42));
    }

    // --- readOnly / writeOnly ---

    #[test]
    fn generate_object_excludes_write_only_properties() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("{}");
        let schema = yaml(
            "type: object\n\
             properties:\n\
             \x20 name:\n\
             \x20   type: string\n\
             \x20   example: Alice\n\
             \x20 password:\n\
             \x20   type: string\n\
             \x20   writeOnly: true\n\
             \x20   example: secret",
        );
        let result = generate(&schema, &root, None);
        assert_eq!(result["name"], json!("Alice"));
        assert!(
            result.get("password").is_none(),
            "writeOnly field should not appear in response"
        );
    }

    #[test]
    fn generate_object_includes_read_only_properties() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("{}");
        let schema = yaml(
            "type: object\n\
             properties:\n\
             \x20 id:\n\
             \x20   type: string\n\
             \x20   readOnly: true\n\
             \x20   example: abc-123",
        );
        let result = generate(&schema, &root, None);
        assert_eq!(result["id"], json!("abc-123"));
    }

    #[test]
    fn generate_object_with_mixed_fields_excludes_only_write_only() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("{}");
        let schema = yaml(
            "type: object\n\
             properties:\n\
             \x20 id:\n\
             \x20   type: string\n\
             \x20   readOnly: true\n\
             \x20   example: id-1\n\
             \x20 name:\n\
             \x20   type: string\n\
             \x20   example: Bob\n\
             \x20 password:\n\
             \x20   type: string\n\
             \x20   writeOnly: true\n\
             \x20   example: secret",
        );
        let result = generate(&schema, &root, None);
        assert_eq!(result["id"], json!("id-1"));
        assert_eq!(result["name"], json!("Bob"));
        assert!(
            result.get("password").is_none(),
            "writeOnly field should not appear in response"
        );
    }

    // --- default fallback ---

    #[test]
    fn generate_uses_default_when_no_example_present() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("{}");
        let schema = yaml("type: string\ndefault: pending");
        assert_eq!(generate(&schema, &root, None), json!("pending"));
    }

    #[test]
    fn generate_prefers_example_over_default() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("{}");
        let schema = yaml("type: string\ndefault: pending\nexample: active");
        assert_eq!(generate(&schema, &root, None), json!("active"));
    }

    #[test]
    fn generate_falls_back_to_random_when_neither_example_nor_default() {
        let root = yaml("{}");
        let schema = yaml("type: integer");
        let result = generate(&schema, &root, None);
        assert!(result.is_number());
    }

    #[test]
    fn generate_default_works_for_integer_type() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("{}");
        let schema = yaml("type: integer\ndefault: 20");
        assert_eq!(generate(&schema, &root, None), json!(20));
    }

    #[test]
    fn generate_does_not_use_default_when_use_examples_is_false() {
        let _guard = UseExamplesGuard;
        set_use_examples(false);
        let root = yaml("{}");
        let result = generate(&yaml("type: string\ndefault: pending"), &root, None);
        assert_ne!(result, json!("pending"));
    }

    // --- nullable ---

    #[test]
    fn generate_nullable_field_sometimes_returns_null() {
        let root = yaml("{}");
        let schema = yaml("type: string\nnullable: true");
        let results: Vec<_> = (0..50).map(|_| generate(&schema, &root, None)).collect();
        assert!(
            results.iter().any(|v| v.is_null()),
            "expected at least one null in 50 runs for a nullable field"
        );
    }

    #[test]
    fn generate_nullable_field_sometimes_returns_the_declared_type() {
        let root = yaml("{}");
        let schema = yaml("type: string\nnullable: true");
        let results: Vec<_> = (0..50).map(|_| generate(&schema, &root, None)).collect();
        assert!(
            results.iter().any(|v| v.is_string()),
            "expected at least one string in 50 runs for a nullable string field"
        );
    }

    #[test]
    fn generate_non_nullable_field_never_returns_null() {
        let root = yaml("{}");
        let schema = yaml("type: string");
        let results: Vec<_> = (0..50).map(|_| generate(&schema, &root, None)).collect();
        assert!(
            results.iter().all(|v| !v.is_null()),
            "non-nullable field should never generate null"
        );
    }

    #[test]
    fn generate_nullable_integer_sometimes_returns_null() {
        let root = yaml("{}");
        let schema = yaml("type: integer\nnullable: true");
        let results: Vec<_> = (0..50).map(|_| generate(&schema, &root, None)).collect();
        assert!(
            results.iter().any(|v| v.is_null()),
            "expected at least one null in 50 runs for a nullable integer field"
        );
    }

    // --- string length constraints ---

    #[test]
    fn generate_string_respects_min_length() {
        let root = yaml("{}");
        let schema = yaml("type: string\nminLength: 100");
        let result = generate(&schema, &root, None);
        let s = result.as_str().expect("generated value should be a string");
        assert!(
            s.len() >= 100,
            "expected length >= 100, got {} ({s:?})",
            s.len()
        );
    }

    #[test]
    fn generate_string_respects_max_length() {
        let root = yaml("{}");
        let schema = yaml("type: string\nmaxLength: 2");
        let result = generate(&schema, &root, None);
        let s = result.as_str().expect("generated value should be a string");
        assert!(
            s.len() <= 2,
            "expected length <= 2, got {} ({s:?})",
            s.len()
        );
    }

    #[test]
    fn generate_string_respects_both_min_and_max_length() {
        let root = yaml("{}");
        let schema = yaml("type: string\nminLength: 10\nmaxLength: 15");
        let result = generate(&schema, &root, None);
        let s = result.as_str().expect("generated value should be a string");
        assert!(
            s.len() >= 10 && s.len() <= 15,
            "expected 10 <= length <= 15, got {} ({s:?})",
            s.len()
        );
    }

    // --- numeric range constraints ---

    #[test]
    fn generate_integer_respects_minimum() {
        let root = yaml("{}");
        let schema = yaml("type: integer\nminimum: 5000");
        let result = generate(&schema, &root, None);
        let n = result
            .as_i64()
            .expect("generated value should be an integer");
        assert!(n >= 5000, "expected n >= 5000, got {n}");
    }

    #[test]
    fn generate_integer_respects_maximum() {
        let root = yaml("{}");
        let schema = yaml("type: integer\nmaximum: 0");
        let result = generate(&schema, &root, None);
        let n = result
            .as_i64()
            .expect("generated value should be an integer");
        assert!(n <= 0, "expected n <= 0, got {n}");
    }

    #[test]
    fn generate_integer_respects_both_minimum_and_maximum() {
        let root = yaml("{}");
        let schema = yaml("type: integer\nminimum: 10\nmaximum: 20");
        let result = generate(&schema, &root, None);
        let n = result
            .as_i64()
            .expect("generated value should be an integer");
        assert!((10..=20).contains(&n), "expected 10 <= n <= 20, got {n}");
    }

    #[test]
    fn generate_number_respects_minimum() {
        let root = yaml("{}");
        let schema = yaml("type: number\nminimum: 5000");
        let result = generate(&schema, &root, None);
        let n = result.as_f64().expect("generated value should be a number");
        assert!(n >= 5000.0, "expected n >= 5000.0, got {n}");
    }

    #[test]
    fn generate_number_respects_maximum() {
        let root = yaml("{}");
        let schema = yaml("type: number\nmaximum: 0");
        let result = generate(&schema, &root, None);
        let n = result.as_f64().expect("generated value should be a number");
        assert!(n <= 0.0, "expected n <= 0.0, got {n}");
    }

    // --- additionalProperties ---

    #[test]
    fn generate_object_with_additional_properties_schema_returns_non_empty_object() {
        let root = yaml("{}");
        let schema = yaml("type: object\nadditionalProperties:\n  type: string");
        let result = generate(&schema, &root, None);
        assert!(result.as_object().map(|m| !m.is_empty()).unwrap_or(false));
    }

    #[test]
    fn generate_object_additional_properties_string_schema_produces_string_values() {
        let root = yaml("{}");
        let schema = yaml("type: object\nadditionalProperties:\n  type: string");
        let result = generate(&schema, &root, None);
        let obj = result.as_object().unwrap();
        assert!(!obj.is_empty(), "expected at least one entry");
        for value in obj.values() {
            assert!(value.is_string(), "expected string value, got {value:?}");
        }
    }

    #[test]
    fn generate_object_additional_properties_integer_schema_produces_integer_values() {
        let root = yaml("{}");
        let schema = yaml("type: object\nadditionalProperties:\n  type: integer");
        let result = generate(&schema, &root, None);
        let obj = result.as_object().unwrap();
        assert!(!obj.is_empty(), "expected at least one entry");
        for value in obj.values() {
            assert!(value.is_i64(), "expected integer value, got {value:?}");
        }
    }

    #[test]
    fn generate_object_named_properties_and_additional_properties_includes_both() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("{}");
        let schema = yaml(
            "type: object\n\
             properties:\n\
             \x20 name:\n\
             \x20   type: string\n\
             \x20   example: Alice\n\
             additionalProperties:\n\
             \x20 type: string",
        );
        let result = generate(&schema, &root, None);
        assert_eq!(
            result["name"],
            json!("Alice"),
            "named property should be present"
        );
        let obj = result.as_object().unwrap();
        assert!(
            obj.len() > 1,
            "expected additional entries beyond named properties"
        );
    }

    #[test]
    fn generate_object_additional_properties_false_returns_only_named_properties() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("{}");
        let schema = yaml(
            "type: object\n\
             properties:\n\
             \x20 name:\n\
             \x20   type: string\n\
             \x20   example: Alice\n\
             additionalProperties: false",
        );
        let result = generate(&schema, &root, None);
        let obj = result.as_object().unwrap();
        assert_eq!(obj.len(), 1);
        assert_eq!(result["name"], json!("Alice"));
    }

    // --- deep nesting (stack overflow regression) ---

    #[test]
    fn generate_does_not_overflow_when_property_has_deeply_nested_example() {
        use yaml_serde::Value as YV;

        let _guard = UseExamplesGuard;
        set_use_examples(true);

        let mut example = YV::Null;
        for _ in 0..500 {
            let mut m = yaml_serde::Mapping::new();
            m.insert(YV::String("child".into()), example);
            example = YV::Mapping(m);
        }

        let mut prop_schema = yaml_serde::Mapping::new();
        prop_schema.insert(YV::String("type".into()), YV::String("string".into()));
        prop_schema.insert(YV::String("example".into()), example);

        let mut properties = yaml_serde::Mapping::new();
        properties.insert(YV::String("data".into()), YV::Mapping(prop_schema));

        let mut schema_map = yaml_serde::Mapping::new();
        schema_map.insert(YV::String("type".into()), YV::String("object".into()));
        schema_map.insert(YV::String("properties".into()), YV::Mapping(properties));

        let schema = YV::Mapping(schema_map);
        let root = YV::Mapping(yaml_serde::Mapping::new());

        let result = generate(&schema, &root, None);
        assert!(
            result.is_object(),
            "object with deeply nested example should generate without overflow"
        );
    }

    #[test]
    fn generate_does_not_overflow_on_deep_ref_chain() {
        let depth = 200;
        let mut root_yaml = String::from("S0:\n  type: string\n");
        for i in 1..=depth {
            root_yaml.push_str(&format!(
                "S{i}:\n  type: object\n  properties:\n    child:\n      $ref: '#/S{}'\n",
                i - 1
            ));
        }
        let root = yaml(&root_yaml);
        let schema = yaml(&format!("$ref: '#/S{depth}'"));

        let result = generate(&schema, &root, None);
        assert!(
            result.is_object(),
            "200-level $ref chain should generate without overflow"
        );
    }

    // --- circular $ref ---

    #[test]
    fn generate_does_not_loop_on_circular_ref() {
        let root = yaml(
            "company:\n\
             \x20 type: object\n\
             \x20 properties:\n\
             \x20   name:\n\
             \x20     type: string\n\
             \x20   mother:\n\
             \x20     $ref: '#/company'",
        );
        let schema = yaml("$ref: '#/company'");
        let result = generate(&schema, &root, None);
        assert!(
            result.is_object(),
            "circular $ref should terminate and produce an object"
        );
    }

    #[test]
    fn generate_allows_one_extra_level_for_circular_ref() {
        let root = yaml(
            "company:\n\
             \x20 type: object\n\
             \x20 properties:\n\
             \x20   name:\n\
             \x20     type: string\n\
             \x20   mother:\n\
             \x20     $ref: '#/company'",
        );
        let schema = yaml("$ref: '#/company'");
        let result = generate(&schema, &root, None);
        assert!(
            result["mother"].is_object(),
            "mother should be a real company object"
        );
        assert!(
            !result["mother"]["mother"].is_object(),
            "mother.mother should be cut off (not a nested company object)"
        );
    }

    // --- forced variant ---

    #[test]
    fn generate_forced_variant_picks_correct_schema() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml(
            "A:\n  type: object\n  properties:\n    k:\n      example: a\n\
             B:\n  type: object\n  properties:\n    k:\n      example: b",
        );
        let schema = yaml(
            "oneOf:\n\
             \x20 - $ref: '#/A'\n\
             \x20 - $ref: '#/B'\n\
             discriminator:\n\
             \x20 propertyName: k\n\
             \x20 mapping:\n\
             \x20   a: '#/A'\n\
             \x20   b: '#/B'",
        );
        assert_eq!(generate(&schema, &root, Some("b"))["k"], json!("b"));
        assert_eq!(generate(&schema, &root, Some("a"))["k"], json!("a"));
    }

    // --- exit_ref: ref count is decremented so the same ref can be visited across siblings ---

    #[test]
    fn generate_object_with_same_ref_in_three_properties_all_resolve() {
        let _guard = UseExamplesGuard;
        set_use_examples(true);
        let root = yaml("Tag:\n  type: string\n  example: label");
        let schema = yaml(
            "type: object\n\
             properties:\n\
             \x20 a:\n\
             \x20   $ref: '#/Tag'\n\
             \x20 b:\n\
             \x20   $ref: '#/Tag'\n\
             \x20 c:\n\
             \x20   $ref: '#/Tag'",
        );
        let result = generate(&schema, &root, None);
        assert_eq!(result["a"], json!("label"));
        assert_eq!(result["b"], json!("label"));
        assert_eq!(result["c"], json!("label"));
    }
}
