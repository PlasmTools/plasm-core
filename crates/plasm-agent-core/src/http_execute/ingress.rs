//! Program body parsing and session plugin execute options.

use super::*;

pub(crate) fn parse_execute_program_body(
    content_type: Option<&str>,
    raw: &[u8],
) -> Result<String, String> {
    let mime = content_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    if mime == "application/json" || mime.ends_with("+json") {
        let v: serde_json::Value =
            serde_json::from_slice(raw).map_err(|e| format!("invalid JSON body: {e}"))?;
        if v.is_array() {
            return Err(
                "JSON top-level array of strings is not supported; send one program string or {\"program\": \"...\"}"
                    .into(),
            );
        }
        if let Some(s) = v.as_str() {
            let t = s.trim();
            if t.is_empty() {
                return Err("program must be a non-empty string".into());
            }
            return Ok(t.to_string());
        }
        if let Some(obj) = v.as_object() {
            if obj.contains_key("lines") {
                return Err(
                    "JSON {\"lines\": [...]} is not supported; send one program string or {\"program\": \"...\"}"
                        .into(),
                );
            }
            if let Some(p) = obj.get("program").and_then(|x| x.as_str()) {
                let t = p.trim();
                if t.is_empty() {
                    return Err("program must be a non-empty string".into());
                }
                return Ok(t.to_string());
            }
        }
        return Err("JSON body must be a quoted program string or {\"program\": \"...\"}".into());
    }

    let s = std::str::from_utf8(raw).map_err(|e| format!("invalid UTF-8: {e}"))?;
    let program = s.trim();
    if program.is_empty() {
        return Err("program must be non-empty".into());
    }
    Ok(program.to_string())
}

/// Upper bound for valid `e#` indices: number of entities exposed in this session (initial open + expand waves).
fn session_entity_symbol_upper_bound(sess: &ExecuteSession) -> Option<usize> {
    if let Some(exp) = sess.teaching_exposure.as_ref() {
        let n = exp.entities.len();
        return (n > 0).then_some(n);
    }
    let n = sess.entities.len();
    (n > 0).then_some(n)
}

/// If the parser reports an unknown entity token, note how many `e#` symbols exist this session.
pub(crate) fn augment_unknown_entity_parse_error(msg: String, sess: &ExecuteSession) -> String {
    if !msg.contains("unknown entity") {
        return msg;
    }
    let Some(k) = session_entity_symbol_upper_bound(sess) else {
        return msg;
    };
    format!(
        "{msg} (this session defines entity symbols e1..e{k} only; use the current Plasm instructions in the `prompt` field)"
    )
}

/// Parser diagnostic plus imperative correction (same pipeline as REPL), for MCP/HTTP tool errors.
pub(crate) fn execute_session_parse_error_message(
    err: &expr_parser::ParseError,
    expanded: &str,
    source_line: &str,
    cgs: &CGS,
    sym_map: &SymbolMap,
) -> String {
    let step = render_parse_error_with_feedback(
        err,
        expanded,
        source_line,
        cgs,
        FeedbackStyle::SymbolicLlm { map: sym_map },
    );
    format!("{err}\n\n{}", step.correction)
}
