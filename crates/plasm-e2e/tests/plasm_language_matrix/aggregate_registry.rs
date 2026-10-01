//! Descriptor occurrence and field arity are witnessed in typed reduction IL.
use plasm_agent::plasm_compile::PythonAggregateDescriptor;
use plasm_core::plasm_monad::{AggregateFunction, ComputeOp, PlasmComp, PlasmStepPayload};
use std::collections::BTreeSet;
fn evidence(comp: &PlasmComp) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for step in comp.steps.values() {
        match step {
            PlasmStepPayload::Map(p) => {
                if let ComputeOp::Aggregate { aggregates } | ComputeOp::GroupBy { aggregates, .. } =
                    &p.compute.op
                {
                    for descriptor in aggregates {
                        let name = match descriptor.function {
                            AggregateFunction::Count => "count",
                            AggregateFunction::Sum => "sum",
                            AggregateFunction::Avg => "avg",
                            AggregateFunction::Min => "min",
                            AggregateFunction::Max => "max",
                            AggregateFunction::First => "first",
                            AggregateFunction::Last => "last",
                        };
                        if matches!(descriptor.function, AggregateFunction::Count)
                            == descriptor.field.is_none()
                        {
                            found.insert(name.into());
                        }
                    }
                }
            }
            PlasmStepPayload::MapBody(body) => found.extend(evidence(&body.body)),
            _ => {}
        }
    }
    found
}
#[tokio::test]
async fn aggregate_dispatch_requires_typed_witnesses_and_rejections() {
    super::constructor_evidence::assert_registry(
        include_str!("../../../../doc-site/docs/reference/python-aggregate-constructors.md"),
        "plasm-aggregate-constructors",
        &PythonAggregateDescriptor::ALL
            .iter()
            .map(|op| op.name())
            .collect::<Vec<_>>(),
        |comp, _| evidence(comp),
    )
    .await;
}
