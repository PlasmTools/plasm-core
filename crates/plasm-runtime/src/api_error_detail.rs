//! Portable extraction of human-readable text from HTTP API error bodies.
//!
//! Pipeline: **media type** (handled in [`crate::http_transport::parse_http_response`]) →
//! **JSON property rules** (this module) or **bounded plain text** → **character cap** so large
//! responses do not flood prompts or MCP tool output.

use serde_json::Value;

/// Maximum length for the API *detail* segment in user-facing [`crate::RuntimeError::RequestError`] messages.
pub const MAX_API_ERROR_DETAIL_CHARS: usize = 768;

/// Larger bound for `tracing::debug!` body previews only (not user-facing).
pub const MAX_DEBUG_BODY_PREVIEW_CHARS: usize = 4096;

/// Truncate `s` to at most `max_chars` Unicode scalars, with a trailing `…` when truncated.
pub fn cap_detail(s: &str, max_chars: usize) -> String {
    let t = s.trim();
    let count = t.chars().count();
    if count <= max_chars {
        return t.to_string();
    }
    let take = max_chars.saturating_sub(1);
    let mut out: String = t.chars().take(take).collect();
    out.push('…');
    out
}

/// Ordered lines for **correction** / multi-line hints (Atlassian field errors, multiple messages).
/// Stops filling once a **primary** shape yields content where appropriate; see implementation.
pub fn json_api_error_lines(value: &Value) -> Vec<String> {
    let mut lines = Vec::new();

    if let Some(s) = value
        .get("message")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        lines.push(s.to_string());
        return lines;
    }

    if let Some(arr) = value.get("errorMessages").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
                lines.push(s.to_string());
            }
        }
        if !lines.is_empty() {
            return lines;
        }
    }

    if let Some(obj) = value.get("errors").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
                lines.push(format!("{k}: {s}"));
            }
        }
        if !lines.is_empty() {
            return lines;
        }
    }

    if let Some(e) = value
        .get("error")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        let mut line = e.to_string();
        if let Some(d) = value
            .get("error_description")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            line.push_str(": ");
            line.push_str(d);
        }
        lines.push(line);
        return lines;
    }

    if let Some(s) = value.as_str().filter(|s| !s.is_empty()) {
        lines.push(s.to_string());
    }

    lines
}

/// GraphQL responses: `{ "errors": [ { "message": "…" } ], "data": null }` when auth fails, validation fails, or a field cannot be resolved.
/// Used to augment decode-path errors — `data` is often `null`, so walking `data.issue` fails with a misleading "missing segment" unless `errors` is shown.
pub fn graphql_errors_summary(value: &Value) -> Option<String> {
    let arr = value.get("errors")?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for e in arr {
        if let Some(msg) = e.get("message").and_then(|m| m.as_str()).map(str::trim) {
            if !msg.is_empty() {
                parts.push(msg.to_string());
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join("; ");
    Some(cap_detail(&joined, MAX_API_ERROR_DETAIL_CHARS))
}

/// Descend one segment of a CML `items_path` (numeric index or object key).
pub fn response_path_step<'a>(cur: &'a Value, key: &str) -> Option<&'a Value> {
    if let Ok(index) = key.parse::<usize>() {
        cur.get(index)
    } else {
        cur.get(key)
    }
}

/// Parent object immediately before the terminal entity segment in `items_path`.
pub fn response_items_path_prefix<'a>(value: &'a Value, items_path: &[String]) -> Option<&'a Value> {
    if items_path.len() < 2 {
        return None;
    }
    let mut cur = value;
    for key in &items_path[..items_path.len() - 1] {
        cur = response_path_step(cur, key)?;
    }
    Some(cur)
}

/// GraphQL **mutation** envelopes commonly wrap the entity in a status object, e.g.
/// `{ data: { issueCreate: { success: false, issue: null } } }`. A `success: false` here is a
/// business-level failure even though the HTTP round-trip and GraphQL parse both succeeded. Walk the
/// `items_path` **prefix** (every segment except the trailing entity key) and, when the wrapper
/// reports `success: false`, return an imperative error instead of letting `items_path` narrowing
/// fail later with an opaque "missing path segment" once the entity is null.
///
/// Returns `None` when there is no such wrapper, when `success` is absent/non-bool, or when
/// `success: true` — leaving the normal decode path untouched.
pub fn graphql_mutation_envelope_failure(value: &Value, items_path: &[String]) -> Option<String> {
    let obj = response_items_path_prefix(value, items_path)?.as_object()?;
    if obj.get("success")?.as_bool()? {
        // success: true (or non-bool handled by the `?` above) → not a failure envelope.
        return None;
    }
    // Business-level failure. Prefer explicit GraphQL `errors`; then common nested error fields on
    // the envelope; else a generic, actionable line. Never leak the items_path segment.
    if let Some(gs) = graphql_errors_summary(value) {
        return Some(cap_detail(
            &format!("write rejected (success: false) — GraphQL: {gs}"),
            MAX_API_ERROR_DETAIL_CHARS,
        ));
    }
    for k in ["error", "message", "userError", "userErrors", "errors"] {
        match obj.get(k) {
            Some(Value::String(s)) if !s.trim().is_empty() => {
                return Some(cap_detail(
                    &format!("write rejected (success: false): {}", s.trim()),
                    MAX_API_ERROR_DETAIL_CHARS,
                ));
            }
            Some(v @ (Value::Array(_) | Value::Object(_))) => {
                if let Ok(detail) = serde_json::to_string(v) {
                    if detail != "[]" && detail != "{}" {
                        return Some(cap_detail(
                            &format!("write rejected (success: false): {detail}"),
                            MAX_API_ERROR_DETAIL_CHARS,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    Some(
        "write rejected by the API (success: false) — check required inputs and permissions for this mutation"
            .to_string(),
    )
}

/// Fibery `/api/commands` envelope: when `success` is false, `result` is an error object (not an array).
/// Helps explain "missing path segment `0`" when narrowing assumed a query row array.
pub fn fibery_command_envelope_hint(value: &Value, missing_segment: &str) -> Option<String> {
    if missing_segment != "0" {
        return None;
    }
    let success = value.get("success")?.as_bool()?;
    if success {
        return None;
    }
    let result = value.get("result")?;
    let name = result
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("command.error");
    let message = result
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("Fibery command failed");
    Some(format!("Fibery command failed ({name}): {message}"))
}

/// Single bounded string for logs and `RuntimeError` (joins [`json_api_error_lines`] with `; `).
pub fn summarize_json_error_body(value: &Value) -> String {
    let lines = json_api_error_lines(value);
    let joined = if lines.is_empty() {
        serde_json::to_string(value).unwrap_or_default()
    } else {
        lines.join("; ")
    };
    cap_detail(&joined, MAX_API_ERROR_DETAIL_CHARS)
}

/// JSON error summary for HTTP client messages: [`summarize_json_error_body`], optional `documentation_url`, then cap.
pub fn summarize_json_api_error_for_http(value: &Value) -> String {
    let mut detail = summarize_json_error_body(value);
    if let Some(doc) = value
        .get("documentation_url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        detail.push_str(&format!(" ({doc})"));
    }
    cap_detail(&detail, MAX_API_ERROR_DETAIL_CHARS)
}

/// Map control characters to spaces (keep `\n` / `\t`) — shared by debug previews and plain-text error paths.
pub fn sanitize_preview_chars(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Non-JSON error body (e.g. HTML): lossy UTF-8, strip most controls, cap.
pub fn summarize_text_error_body(bytes: &[u8], _content_type: Option<&str>) -> String {
    let s = String::from_utf8_lossy(bytes);
    let t = s.trim();
    let cleaned = sanitize_preview_chars(t);
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    cap_detail(&collapsed, MAX_API_ERROR_DETAIL_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fibery_command_envelope_hint_on_success_false() {
        let v = json!({
            "success": false,
            "result": {
                "name": "entity.error/foo",
                "message": "bar"
            }
        });
        let hint = fibery_command_envelope_hint(&v, "0").expect("hint");
        assert!(hint.contains("entity.error/foo"));
        assert!(hint.contains("bar"));
    }

    #[test]
    fn graphql_mutation_envelope_failure_on_success_false() {
        let v = json!({ "data": { "issueCreate": { "success": false, "issue": null } } });
        let path = vec![
            "data".to_string(),
            "issueCreate".to_string(),
            "issue".to_string(),
        ];
        let msg = graphql_mutation_envelope_failure(&v, &path).expect("failure");
        assert!(msg.contains("success: false"), "{msg}");
        assert!(!msg.contains("missing path segment"), "{msg}");
    }

    #[test]
    fn graphql_mutation_envelope_failure_prefers_top_level_errors() {
        let v = json!({
            "data": { "commentCreate": { "success": false, "comment": null } },
            "errors": [{ "message": "Argument Validation Error" }]
        });
        let path = vec![
            "data".to_string(),
            "commentCreate".to_string(),
            "comment".to_string(),
        ];
        let msg = graphql_mutation_envelope_failure(&v, &path).expect("failure");
        assert!(msg.contains("Argument Validation Error"), "{msg}");
    }

    #[test]
    fn graphql_mutation_envelope_failure_none_on_success_true() {
        let v = json!({ "data": { "issueCreate": { "success": true, "issue": { "id": "x" } } } });
        let path = vec![
            "data".to_string(),
            "issueCreate".to_string(),
            "issue".to_string(),
        ];
        assert!(graphql_mutation_envelope_failure(&v, &path).is_none());
    }

    #[test]
    fn graphql_mutation_envelope_failure_none_without_success_field() {
        // Plain query narrowing (`[data, issue]`) must not be mistaken for a mutation envelope.
        let v = json!({ "data": { "issue": null } });
        let path = vec!["data".to_string(), "issue".to_string()];
        assert!(graphql_mutation_envelope_failure(&v, &path).is_none());
    }

    #[test]
    fn graphql_mutation_envelope_failure_none_on_array_prefix() {
        // `[data, teams, nodes, "0"]` prefix lands on an array, not a status object.
        let v = json!({ "data": { "teams": { "nodes": [] } } });
        let path = vec![
            "data".to_string(),
            "teams".to_string(),
            "nodes".to_string(),
            "0".to_string(),
        ];
        assert!(graphql_mutation_envelope_failure(&v, &path).is_none());
    }

    #[test]
    fn github_style_message() {
        let v = json!({"message": "Not Found", "documentation_url": "https://docs.github.com"});
        assert_eq!(json_api_error_lines(&v), vec!["Not Found"]);
        let s = summarize_json_error_body(&v);
        assert!(s.contains("Not Found"));
        assert!(s.len() <= MAX_API_ERROR_DETAIL_CHARS + 16);
    }

    #[test]
    fn summarize_json_api_error_for_http_appends_doc_url() {
        let v = json!({"message": "Not Found", "documentation_url": "https://docs.github.com/x"});
        let s = summarize_json_api_error_for_http(&v);
        assert!(s.contains("Not Found"));
        assert!(s.contains("docs.github.com"));
    }

    #[test]
    fn atlassian_error_messages() {
        let v = json!({
            "errorMessages": ["No project could be found with key 'POKE'."],
            "errors": {}
        });
        let lines = json_api_error_lines(&v);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("POKE"));
    }

    #[test]
    fn atlassian_errors_map_when_no_messages() {
        let v = json!({
            "errorMessages": [],
            "errors": {"project": "invalid"}
        });
        let lines = json_api_error_lines(&v);
        assert_eq!(lines, vec!["project: invalid"]);
    }

    #[test]
    fn oauth_style() {
        let v = json!({
            "error": "invalid_request",
            "error_description": "Missing parameter"
        });
        assert_eq!(
            json_api_error_lines(&v),
            vec!["invalid_request: Missing parameter"]
        );
    }

    #[test]
    fn message_wins_over_error_messages() {
        let v = json!({
            "message": "primary",
            "errorMessages": ["other"]
        });
        assert_eq!(json_api_error_lines(&v), vec!["primary"]);
    }

    #[test]
    fn graphql_errors_array_messages() {
        let v = json!({
            "errors": [{ "message": "Variable \"$id\" got invalid value" }],
            "data": null
        });
        let s = graphql_errors_summary(&v).expect("summary");
        assert!(s.contains("invalid value"), "{s}");
    }

    #[test]
    fn cap_detail_truncates() {
        let s: String = (0..900).map(|_| 'x').collect();
        let c = cap_detail(&s, 100);
        assert_eq!(c.chars().count(), 100);
        assert!(c.ends_with('…'));
    }

    #[test]
    fn summarize_text_strips_htmlish_noise() {
        let bytes = b"<!DOCTYPE html><html><body>oops</body></html>";
        let s = summarize_text_error_body(bytes, Some("text/html"));
        assert!(s.contains("oops") || s.contains("<!DOCTYPE"));
        assert!(s.len() <= MAX_API_ERROR_DETAIL_CHARS + 4);
    }

    #[test]
    fn response_path_step_index_and_key() {
        let v = json!({ "data": [{ "id": "x" }, { "id": "y" }] });
        let row0 = response_path_step(&v, "data")
            .and_then(|data| response_path_step(data, "0"))
            .expect("index step");
        assert_eq!(row0.get("id").and_then(|v| v.as_str()), Some("x"));
        let row1 = response_path_step(&v, "data")
            .and_then(|data| response_path_step(data, "1"))
            .expect("index step");
        assert_eq!(row1.get("id").and_then(|v| v.as_str()), Some("y"));
    }
}
