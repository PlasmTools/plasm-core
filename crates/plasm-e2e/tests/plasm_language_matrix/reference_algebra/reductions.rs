//! Finite reduction products through the shared live differential harness.
use super::{compare_program, model::*, reduction_model, server};
use serde_json::json;
use std::collections::BTreeMap;

pub(in super::super) async fn run() -> usize {
    let datasets: &[&[Option<i64>]] = &[
        &[],
        &[Some(3)],
        &[None],
        &[None, None, None],
        &[None, Some(-2), Some(4), None],
        &[Some(4), Some(-2), Some(4), Some(0)],
    ];
    let mut count = 0;
    for (index, values) in datasets.iter().enumerate() {
        let source = Rows {
            shape: BTreeMap::from([
                ("id".into(), Type::Text),
                ("title".into(), Type::Text),
                ("score".into(), Type::Nullable(Box::new(Type::Integer))),
            ]),
            observed: false,
            coverage: Coverage::Complete,
            continuation: false,
            values: values
                .iter()
                .enumerate()
                .map(|(n, value)| {
                    serde_json::from_value(json!({
                "id":format!("i{n}"), "title":if n % 2 == 0 { "z" } else { "a" }, "score":value
            })).unwrap()
                })
                .collect(),
        };
        let server = server(&source).await;
        for descending in [false, true] {
            let mut ordered = source.values.clone();
            if descending {
                ordered.reverse();
            }
            for grouped in [false, true] {
                let expected = reduction_model::reduce(&ordered, grouped);
                for alias in [false, true] {
                    let field = if alias { "points" } else { "score" };
                    let projection = if alias {
                        ".select(\"title\", points=\"score\")"
                    } else {
                        ""
                    };
                    let reduction = if grouped {
                        "group_by(\"title\", "
                    } else {
                        "aggregate("
                    };
                    let body = format!("return E.query().order_by(\"id\", descending={}){projection}.{reduction}n=agg.count(), total=agg.sum(\"{field}\"), mean=agg.avg(\"{field}\"), lo=agg.min(\"{field}\"), hi=agg.max(\"{field}\"), first_value=agg.first(\"{field}\"), last_value=agg.last(\"{field}\"))", if descending { "True" } else { "False" });
                    compare_program(|entity| super::super::python::program(&body, entity), &expected, &server.base)
                        .await.unwrap_or_else(|error| panic!("reduction-v1 dataset={index} descending={descending} grouped={grouped} alias={alias}: {error:#?}"));
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 48);
    count
}
