//! Stable program-surface error messages.

use super::flatten::leading_identifier;
use super::labels::looks_like_domain_symbol;

/// Agent-facing hint when a program has bindings but no executable return roots.
pub fn missing_program_roots_error() -> String {
    "Add a final return line (e.g. `limited[p2,p14]`), or omit only when every line is `label = …` (last binding is returned)."
        .to_string()
}

pub fn program_empty_error() -> String {
    "Program is empty.".to_string()
}

pub fn program_return_keyword_error() -> String {
    "Remove `return` — write bare roots on the last line (e.g. `limited` or `a, b`).".to_string()
}

pub fn program_invalid_binding_label_error(label: &str) -> String {
    if looks_like_domain_symbol(label) {
        format!("Binding names must be labels like `issue`, not teaching symbols (`{label}`).")
    } else {
        format!("Binding names must be identifiers like `issue`, not `{label}`.")
    }
}

pub fn program_binding_after_return_error() -> String {
    "Return must be last — move bindings above the return line, or bind intermediate steps before returning."
        .to_string()
}

pub fn program_multiple_return_lines_error() -> String {
    "Only one return line allowed — put every root on one comma-separated line (e.g. `a, b, c`), not one bare label per line."
        .to_string()
}

pub fn program_intermediate_return_error(_stmt: &str) -> String {
    // Do **not** echo the offending statement: when the agent unrolled N literal
    // applies, quoting them teaches "bind each unroll" instead of fanout.
    "Only one return line allowed — bind intermediate steps \
     (`done = rows => e#.m#(…, _.f)` or `label = …`), then end with one return \
     line of roots."
        .to_string()
}

pub fn program_intermediate_return_must_be_binding_error(stmt: &str) -> String {
    let head = leading_identifier(stmt.trim());
    let bind = if head.is_empty() { "step" } else { head };
    // Prefer a short bind name only — never re-paste the full expression body
    // (may contain copied path/id literals from a prior observation).
    format!(
        "Intermediate step must be a binding — write `{bind} = …` (not a bare \
         expression line), then return on the last line."
    )
}

pub fn program_duplicate_return_node_error() -> String {
    "Program has multiple return expressions — bind each step (`filtered = e# | where …`), then one final return line."
        .to_string()
}
