use crate::PlasmComp;

/// Canonical semantic subset for plan commit / evidence hashing.
pub fn plasm_comp_commit_canonical(comp: &PlasmComp) -> serde_json::Value {
    let mut steps = serde_json::to_value(&comp.steps).expect("serializable comp steps");
    for (id, payload) in &comp.steps {
        if let crate::PlasmStepPayload::MapBody(body) = payload {
            steps[id]["body"] = plasm_comp_commit_canonical(&body.body);
        }
    }
    serde_json::json!({
        "version": comp.version,
        "steps": steps,
        "bind": comp.bind,
        "return": comp.return_,
    })
}
