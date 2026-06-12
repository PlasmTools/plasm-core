use crate::PlasmComp;

/// Canonical semantic subset for plan commit / evidence hashing.
pub fn plasm_comp_commit_canonical(comp: &PlasmComp) -> serde_json::Value {
    serde_json::json!({
        "version": comp.version,
        "steps": comp.steps,
        "bind": comp.bind,
        "return": comp.return_,
    })
}
