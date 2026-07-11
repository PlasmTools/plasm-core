use std::collections::HashMap;

pub const OPENROUTER_OPENAI_COMPAT_BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const DEFAULT_OPENROUTER_EVAL_TEMPERATURE: f64 = 0.0;
pub const DEFAULT_OPENROUTER_EVAL_SEED: u64 = 42;

pub fn openrouter_eval_llm_options(
    model: &str,
    api_key: &str,
    temperature: f64,
    seed: u64,
) -> HashMap<String, serde_json::Value> {
    [
        (
            "base_url".to_string(),
            serde_json::json!(OPENROUTER_OPENAI_COMPAT_BASE_URL),
        ),
        ("model".to_string(), serde_json::json!(model)),
        ("api_key".to_string(), serde_json::json!(api_key)),
        ("temperature".to_string(), serde_json::json!(temperature)),
        ("seed".to_string(), serde_json::json!(seed)),
    ]
    .into_iter()
    .collect()
}

pub fn model_slug(model: &str) -> String {
    model
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}
