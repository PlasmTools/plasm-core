//! Presentation-only elimination of small, structural singleton packets.
//! Executable computes, row scopes and recorded observations are never folded.
use super::*;
use std::collections::{BTreeMap, HashSet};

const MAX_INLINE_BYTES: usize = 240;

pub(super) struct Projection {
    pub names: HashMap<String, String>,
    pub hidden: HashSet<String>,
    dependencies: HashMap<String, Vec<String>>,
    values: HashMap<String, PlanValue>,
}

impl Projection {
    pub fn new(
        nodes: &[ValidatedPlanNode],
        order: &[String],
        aliases: &BTreeMap<String, serde_json::Value>,
    ) -> Self {
        let mut names = build_plan_node_display_map(nodes, order);
        let mut authored = HashSet::new();
        let mut occupied: HashSet<_> = names.values().cloned().collect();
        if let Some(serde_json::Value::Object(bindings)) = aliases.get("python_binding_names") {
            for (id, name) in bindings {
                let Some(name) = name.as_str() else { continue };
                if names.contains_key(id)
                    && is_synthetic_plan_node_id(id)
                    && !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && occupied.insert(name.to_owned())
                {
                    names.insert(id.clone(), name.to_owned());
                    authored.insert(id.clone());
                }
            }
        }
        let by_id: HashMap<_, _> = nodes.iter().map(|n| (n.id().as_str(), n)).collect();
        // Native operators display their operation, not an inline input packet.
        // Keep those inputs named so field/value mappings remain reviewable.
        let mut retained = HashSet::new();
        for node in nodes {
            match node {
                ValidatedPlanNode::Compute(n)
                    if !matches!(n.compute.op, ComputeOp::Python { .. }) =>
                {
                    retained.insert(n.compute.source.clone());
                }
                ValidatedPlanNode::Derive(n)
                    if structural_value(&n.value, &names, &HashMap::new(), 0).is_none() =>
                {
                    retained.insert(n.source.to_string());
                    retained.extend(step_upstream_labels(node, &HashMap::new()));
                }
                _ => {}
            }
        }
        let units: HashSet<_> = nodes
            .iter()
            .filter_map(|node| match node {
                ValidatedPlanNode::Data(n)
                    if n.recorded_output.is_none()
                        && n.uses_result.is_empty()
                        && matches!(&n.data, PlanValue::Literal { value }
                        if value.to_wire().ok().is_some_and(|v| v == serde_json::json!({}))) =>
                {
                    Some(n.id.as_str())
                }
                _ => None,
            })
            .collect();
        let mut result = Self {
            names,
            hidden: HashSet::new(),
            dependencies: HashMap::new(),
            values: HashMap::new(),
        };
        for id in order {
            let Some(node) = by_id.get(id.as_str()) else {
                continue;
            };
            let mut dependencies = Vec::new();
            // depends_on also includes scheduling barriers. Display data uses
            // here; do not invent a verification dependency from source order.
            for dependency in step_upstream_labels(node, &HashMap::new()) {
                if result.hidden.contains(&dependency) {
                    dependencies.extend(
                        result
                            .dependencies
                            .get(&dependency)
                            .into_iter()
                            .flatten()
                            .cloned(),
                    );
                } else {
                    dependencies.push(dependency);
                }
            }
            let mut seen = HashSet::new();
            dependencies.retain(|id| seen.insert(id.clone()));
            result.dependencies.insert(id.clone(), dependencies);
            if !is_synthetic_plan_node_id(id) || authored.contains(id) || retained.contains(id) {
                continue;
            }
            let expression = match node {
                ValidatedPlanNode::Data(_) if units.contains(id.as_str()) => Some("{}".to_owned()),
                ValidatedPlanNode::Derive(n) if units.contains(n.source.as_str()) => {
                    structural_value(&n.value, &result.names, &result.values, 0)
                }
                _ => None,
            };
            if let Some(expression) = expression.filter(|v| v.len() <= MAX_INLINE_BYTES) {
                result.names.insert(id.clone(), expression);
                result.hidden.insert(id.clone());
                if let ValidatedPlanNode::Derive(n) = node {
                    result.values.insert(id.clone(), n.value.clone());
                }
            }
        }
        result
    }

    pub fn uses(&self, node: &ValidatedPlanNode) -> Vec<String> {
        self.dependencies
            .get(node.id().as_str())
            .into_iter()
            .flatten()
            .map(|id| map_display_id(id, &self.names))
            .collect()
    }

    pub fn op(&self, node: &ValidatedPlanNode, es: Option<&ExecuteSession>) -> PlanDryOp {
        let mut op = compact_op_from_node(node, es, &self.names);
        if let (ValidatedPlanNode::Compute(n), PlanDryOp::Python { input, .. }) = (node, &mut op) {
            if self.hidden.contains(&n.compute.source) {
                *input = Some(map_display_id(&n.compute.source, &self.names));
            }
        }
        // Show authored structural bindings as values rather than field counts.
        if let (ValidatedPlanNode::Derive(n), PlanDryOp::Derive { summary, .. }) = (node, &mut op) {
            // A retained packet may exceed the folding threshold: show its
            // complete mapping once rather than falling back to field counts.
            if let Some(value) = structural_value(&n.value, &self.names, &HashMap::new(), 0) {
                *summary = value;
            }
        }
        op
    }
}

fn structural_value(
    value: &PlanValue,
    names: &HashMap<String, String>,
    values: &HashMap<String, PlanValue>,
    depth: usize,
) -> Option<String> {
    if depth > 8 {
        return None;
    }
    let rendered = match value {
        PlanValue::Object { fields } => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(key, value)| {
                    Some(format!(
                        "{}: {}",
                        serde_json::to_string(key).ok()?,
                        structural_value(value, names, values, depth + 1)?
                    ))
                })
                .collect::<Option<Vec<_>>>()?
                .join(", ")
        ),
        PlanValue::Array { items } => format!(
            "[{}]",
            items
                .iter()
                .map(|v| structural_value(v, names, values, depth + 1))
                .collect::<Option<Vec<_>>>()?
                .join(", ")
        ),
        PlanValue::NodeSymbol { node, path, .. } => {
            if !path.is_empty() {
                if let Some(selected) = values.get(node).and_then(|value| {
                    path.iter().try_fold(value, |value, field| match value {
                        PlanValue::Object { fields } => fields.get(field),
                        _ => None,
                    })
                }) {
                    return structural_value(selected, names, values, depth + 1);
                }
            }
            let source = map_display_id(node, names);
            if path.is_empty() {
                source
            } else {
                format!("{source}.{}", path.join("."))
            }
        }
        PlanValue::Literal { value } => serde_json::to_string(&value.to_wire().ok()?).ok()?,
        // BindingSymbol is row-scoped, and expressions may fail. Keep those nodes.
        _ => return None,
    };
    Some(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literal(value: serde_json::Value) -> PlanValue {
        PlanValue::Literal {
            value: plasm_core::operand_binding::ResolvedValue::from_wire(value).unwrap(),
        }
    }

    #[test]
    fn native_consumers_keep_structural_input_mapping() {
        for op in [
            serde_json::json!({"kind":"limit","count":1}),
            serde_json::json!({"kind":"project","fields":{"predicate":["predicate"]}}),
            serde_json::json!({"kind":"filter","predicates":{"kind":"atom","args":{"field_path":["predicate"],"op":"eq","value":{"kind":"literal","value":true}}}}),
            serde_json::Value::Null,
        ] {
            let mut plan = serde_json::json!({
                "version":1,"kind":"program",
                "nodes":[
                    {"id":"__py1","kind":"data","effect_class":"artifact_read","result_shape":"single","data":{"kind":"literal","value":{}}},
                    {"id":"__py2","kind":"derive","effect_class":"artifact_read","result_shape":"list","derive_template":{"kind":"map","source":"__py1","item_binding":"unit","value":{"kind":"object","fields":{"predicate":{"kind":"literal","value":true}}}},"depends_on":["__py1"],"uses_result":[{"node":"__py1","as":"unit"}]},
                    {"id":"checked","kind":"compute","effect_class":"artifact_read","result_shape":"list","compute":{"source":"__py2","op":op,"schema":{"entity":"Check","fields":[{"name":"predicate","value_kind":"boolean","source":["predicate"]}]}},"depends_on":["__py2"],"uses_result":[{"node":"__py2","as":"packet"}]}
                ],"return":{"kind":"node","node":"checked"}
            });
            if op.is_null() {
                plan["nodes"][2] = serde_json::json!({"id":"checked","kind":"derive","effect_class":"artifact_read","result_shape":"artifact","derive_template":{"kind":"map","source":"__py2","item_binding":"packet","value":{"kind":"template","template":"{{ packet.predicate }}","input_bindings":[{"from":"packet","to":"packet"}]}},"depends_on":["__py2"],"uses_result":[{"node":"__py2","as":"packet"}]});
            }
            let plan = crate::plasm_plan::parse_and_validate_plan_json(&plan).unwrap();
            let order = plan
                .topological_order()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            let projection = Projection::new(plan.nodes(), &order, &BTreeMap::new());
            assert!(!projection.hidden.contains("__py2"));
            let input = plan
                .nodes()
                .iter()
                .find(|n| n.id().as_str() == "__py2")
                .unwrap();
            let PlanDryOp::Derive { summary, .. } = projection.op(input, None) else {
                panic!("derive")
            };
            assert_eq!(summary, "{\"predicate\": true}");
            let checked = plan
                .nodes()
                .iter()
                .find(|n| n.id().as_str() == "checked")
                .unwrap();
            assert_eq!(
                projection.uses(checked),
                vec![projection.names["__py2"].clone()]
            );
        }
    }

    #[test]
    fn structural_fields_preserve_null_missing_and_empty_values() {
        let names = HashMap::from([("packet".into(), "packet".into())]);
        for value in [
            serde_json::Value::Null,
            serde_json::json!(""),
            serde_json::json!([]),
        ] {
            let values = HashMap::from([(
                "packet".into(),
                PlanValue::Object {
                    fields: BTreeMap::from([("present".into(), literal(value.clone()))]),
                },
            )]);
            for field in ["present", "absent"] {
                let reference = PlanValue::NodeSymbol {
                    node: "packet".into(),
                    alias: "packet".into(),
                    path: vec![field.into()],
                };
                let rendered = structural_value(&reference, &names, &values, 0).unwrap();
                assert_eq!(
                    rendered,
                    if field == "present" {
                        value.to_string()
                    } else {
                        "packet.absent".into()
                    }
                );
            }
        }
    }

    #[test]
    fn folded_inputs_remain_visible_in_each_review_renderer() {
        let op = PlanDryOp::Python {
            entity: "Row".into(),
            per_row: false,
            input: Some("{\"identity\": observed.id, \"note\": null}".into()),
        };
        for rendered in [human_ux_summary_for_op(&op), render_plan_dry_op(&op)] {
            assert!(rendered.contains("observed.id"), "{rendered}");
            assert!(rendered.contains("null"), "{rendered}");
        }
    }

    #[test]
    fn retained_structural_summary_keeps_large_substituted_inputs() {
        let names = HashMap::from([("packet".into(), "x".repeat(MAX_INLINE_BYTES))]);
        let value = PlanValue::Object {
            fields: BTreeMap::from([
                (
                    "first".into(),
                    PlanValue::NodeSymbol {
                        node: "packet".into(),
                        alias: "p".into(),
                        path: vec![],
                    },
                ),
                ("second".into(), literal(serde_json::json!(true))),
            ]),
        };
        let text = structural_value(&value, &names, &HashMap::new(), 0).unwrap();
        assert!(text.len() > MAX_INLINE_BYTES);
        assert!(text.contains(&"x".repeat(MAX_INLINE_BYTES)));
        assert!(text.contains("\"second\": true"));
    }

    #[test]
    fn structural_folding_is_bounded_and_does_not_guess_row_scopes() {
        let names = HashMap::new();
        let values = HashMap::new();
        assert!(
            structural_value(
                &literal(serde_json::json!("x".repeat(MAX_INLINE_BYTES))),
                &names,
                &values,
                0
            )
            .unwrap()
            .len()
                > MAX_INLINE_BYTES
        );
        assert!(structural_value(
            &PlanValue::BindingSymbol {
                binding: "row".into(),
                path: vec!["id".into()]
            },
            &names,
            &values,
            0
        )
        .is_none());
        assert!(structural_value(&literal(serde_json::json!(0)), &names, &values, 9).is_none());
    }

    #[test]
    fn projection_retains_authored_bindings_and_recorded_observations() {
        use crate::plasm_plan::{ResultShape, ValidatedDataNode};
        let data = |id: &str, recorded| {
            ValidatedPlanNode::Data(ValidatedDataNode {
                id: crate::plasm_plan::PlanNodeId::new(id).unwrap(),
                effect_class: EffectClass::Read,
                result_shape: ResultShape::Single,
                data: literal(serde_json::json!({})),
                recorded_output: recorded,
                depends_on: vec![],
                uses_result: vec![],
            })
        };
        let nodes = vec![
            data("__py1", None),
            data("__py2", None),
            data(
                "__py3",
                Some(plasm_core::plasm_monad::RecordedOutputOrigin {
                    run_id: format!("pr{}", "a".repeat(64)),
                    entry_id: "matrix".into(),
                    entity: "Observation".into(),
                    labels: Default::default(),
                    provenance: Default::default(),
                }),
            ),
            data("authored", None),
        ];
        let order = nodes.iter().map(|n| n.id().to_string()).collect::<Vec<_>>();
        let metadata = BTreeMap::from([(
            "python_binding_names".into(),
            serde_json::json!({"__py2": "retained"}),
        )]);
        let projection = Projection::new(&nodes, &order, &metadata);
        assert_eq!(projection.hidden, HashSet::from(["__py1".into()]));
        assert_eq!(projection.names["__py2"], "retained");
        assert_eq!(projection.names["authored"], "authored");
    }
}
