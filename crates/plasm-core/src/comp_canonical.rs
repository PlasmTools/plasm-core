use crate::PlasmComp;

/// Canonical semantic subset for plan commit / evidence hashing.
pub fn plasm_comp_commit_canonical(comp: &PlasmComp) -> serde_json::Value {
    use crate::PlasmStepPayload as Payload;
    enum Work<'a> {
        Comp(&'a PlasmComp),
        FinishComp(&'a PlasmComp),
        Step(&'a Payload),
        FinishStep(&'a Payload),
    }
    let mut pending = vec![Work::Comp(comp)];
    let mut values = Vec::new();
    while let Some(work) = pending.pop() {
        match work {
            Work::Comp(comp) => {
                pending.push(Work::FinishComp(comp));
                pending.extend(comp.steps.values().rev().map(Work::Step));
            }
            Work::Step(payload @ Payload::MapBody(body)) => {
                pending.push(Work::FinishStep(payload));
                pending.push(Work::Comp(&body.body));
            }
            Work::Step(payload @ Payload::UnfoldUntil(unfold)) => {
                pending.push(Work::FinishStep(payload));
                if let Some(body) = &unfold.until_scope {
                    pending.push(Work::Comp(&body.body));
                }
                if let Some(body) = &unfold.step_scope {
                    pending.push(Work::Comp(&body.body));
                }
            }
            Work::Step(payload) => {
                values.push(serde_json::to_value(payload).expect("serializable comp step"))
            }
            Work::FinishStep(Payload::MapBody(body)) => {
                let child = values.pop().expect("canonical body");
                let mut value = serde_json::json!({
                    "kind": "map_body", "parent": body.parent, "output": body.output,
                    "max_parents": body.max_parents, "captures": body.captures, "body": null,
                });
                value["body"] = child;
                values.push(value);
            }
            Work::FinishStep(Payload::UnfoldUntil(unfold)) => {
                let until = unfold
                    .until_scope
                    .as_ref()
                    .map(|_| values.pop().expect("canonical predicate"));
                let step = unfold
                    .step_scope
                    .as_ref()
                    .map(|_| values.pop().expect("canonical step"));
                let mut value = serde_json::json!({
                    "kind": "unfold_until", "source": unfold.source, "item_binding": unfold.item_binding,
                    "effect_template": unfold.effect_template, "until_predicates": unfold.until_predicates,
                    "until_scope": null, "step_scope": null, "take": unfold.take,
                    "seed_ir": unfold.seed_ir, "approval": unfold.approval,
                    "effect_class": unfold.effect_class, "result_shape": unfold.result_shape,
                });
                let object = value.as_object_mut().expect("canonical unfold object");
                if unfold.until_predicates.is_empty() {
                    object.remove("until_predicates");
                }
                if unfold.seed_ir.is_none() {
                    object.remove("seed_ir");
                }
                if unfold.approval.is_none() {
                    object.remove("approval");
                }
                for (name, body, child) in [
                    ("until_scope", unfold.until_scope.as_ref(), until),
                    ("step_scope", unfold.step_scope.as_ref(), step),
                ] {
                    if let Some(body) = body {
                        let mut metadata = serde_json::json!({
                            "parent": body.parent, "output": body.output, "max_parents": body.max_parents,
                            "captures": body.captures, "body": null,
                        });
                        metadata["body"] = child.expect("canonical scope");
                        object.insert(name.to_owned(), metadata);
                    } else {
                        object.remove(name);
                    }
                }
                values.push(value);
            }
            Work::FinishStep(_) => unreachable!("only scoped steps need continuations"),
            Work::FinishComp(comp) => {
                let children = values.split_off(values.len() - comp.steps.len());
                let steps = comp.steps.keys().cloned().zip(children).collect();
                let mut value = serde_json::json!({"version": comp.version, "steps": null, "bind": comp.bind, "return": comp.return_});
                value["steps"] = serde_json::Value::Object(steps);
                values.push(value);
            }
        }
    }
    values.pop().expect("canonical comp root")
}
