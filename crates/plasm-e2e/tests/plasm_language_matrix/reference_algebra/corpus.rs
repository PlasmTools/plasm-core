//! Enumerated finite grammar and a separate source emitter.
use super::model::{Expr, Shape};

pub const VERSION: &str = "read-value-v3";

pub fn cases(shape: &Shape) -> Vec<Expr> {
    let mut all = vec![Expr::Source];
    let mut layer = all.clone();
    for _ in 0..2 {
        let mut next = Vec::new();
        for expression in layer {
            next.extend([
                Expr::Take(Box::new(expression.clone()), 1),
                Expr::OrderId(Box::new(expression.clone()), true),
                Expr::KeepTitle(Box::new(expression.clone()), "Alpha".into()),
                Expr::Record(Box::new(expression)),
            ]);
        }
        all.extend(next.clone());
        layer = next;
    }
    all.push(Expr::Project(
        Box::new(Expr::Source),
        vec!["id".into(), "score".into()],
    ));
    let titles = Expr::Project(Box::new(Expr::Source), vec!["title".into()]);
    all.push(titles.clone());
    all.push(Expr::Union(Box::new(titles.clone()), Box::new(titles)));
    for right in [
        Expr::Source,
        Expr::Take(Box::new(Expr::Source), 1),
        Expr::KeepTitle(Box::new(Expr::Source), "missing".into()),
    ] {
        all.push(Expr::Union(
            Box::new(Expr::Record(Box::new(Expr::Source))),
            Box::new(Expr::Record(Box::new(right))),
        ));
    }
    for parents in [
        Expr::Source,
        Expr::Take(Box::new(Expr::Source), 1),
        Expr::KeepTitle(Box::new(Expr::Source), "missing".into()),
    ] {
        for children in [
            Expr::Source,
            Expr::Take(Box::new(Expr::Source), 1),
            Expr::KeepTitle(Box::new(Expr::Source), "missing".into()),
        ] {
            all.push(Expr::Nest {
                parents: Box::new(parents.clone()),
                children: Box::new(children),
                bound: 8,
                capture: true,
            });
        }
    }
    all.push(Expr::Nest {
        parents: Box::new(Expr::Take(Box::new(Expr::Source), 1)),
        children: Box::new(Expr::Nest {
            parents: Box::new(Expr::Source),
            children: Box::new(Expr::Take(Box::new(Expr::Source), 1)),
            bound: 8,
            capture: true,
        }),
        bound: 8,
        capture: true,
    });
    for children in [
        Expr::Source,
        Expr::Project(Box::new(Expr::Source), vec!["title".into()]),
        Expr::Take(Box::new(Expr::Source), 1),
        Expr::KeepTitle(Box::new(Expr::Source), "missing".into()),
    ] {
        all.push(Expr::Nest {
            parents: Box::new(Expr::Source),
            children: Box::new(children),
            bound: 8,
            capture: false,
        });
    }
    assert_eq!(all.len(), 41, "versioned grammar inventory changed");
    for expression in &all {
        expression.shape(shape).expect("generator must be typed");
    }
    all
}

pub fn source(expression: &Expr, shape: &Shape, entity: &str) -> String {
    let mut next = 0;
    let body = emit(expression, shape, entity, &mut next);
    format!("class AlgebraProgram(Program):\n    def build(self):\n        return {body}\n")
}

fn fresh(next: &mut usize) -> String {
    let name = format!("row{next}");
    *next += 1;
    name
}

fn emit(expr: &Expr, shape: &Shape, entity: &str, next: &mut usize) -> String {
    match expr {
        Expr::ObservedSource => format!("{entity}.query().order_by(\"id\")"),
        Expr::Source => {
            format!("{entity}.query().order_by(\"id\").select(\"id\", \"title\", \"score\")")
        }
        Expr::Project(inner, keys) => format!(
            "{}.select({})",
            emit(inner, shape, entity, next),
            keys.iter()
                .map(|k| serde_json::to_string(k).unwrap())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Take(inner, n) => format!("{}.take({n})", emit(inner, shape, entity, next)),
        Expr::OrderId(inner, descending) => format!(
            "{}.order_by(\"id\", descending={})",
            emit(inner, shape, entity, next),
            if *descending { "True" } else { "False" }
        ),
        Expr::KeepTitle(inner, title) => {
            let source = emit(inner, shape, entity, next);
            let row = fresh(next);
            format!(
                "{source}.where(lambda {row}: {row}.title == {})",
                serde_json::to_string(title).unwrap()
            )
        }
        Expr::Union(a, b) => format!(
            "{}.union({})",
            emit(a, shape, entity, next),
            emit(b, shape, entity, next)
        ),
        Expr::Record(inner) => {
            let source = emit(inner, shape, entity, next);
            let row = fresh(next);
            format!("{source}.map(lambda {row}: {{\"id\":{row}.id,\"title\":{row}.title,\"score\":{row}.score}}, max_parents=8)")
        }
        Expr::Nest {
            parents,
            children,
            bound,
            capture,
        } => {
            let parent_source = emit(parents, shape, entity, next);
            let parent = fresh(next);
            let child_source = emit(children, shape, entity, next);
            if !capture {
                return format!("{parent_source}.map(lambda {parent}: {{\"id\":{parent}.id,\"children\":{child_source}}}, max_parents={bound})");
            }
            let child = fresh(next);
            let mut fields: Vec<_> = children
                .shape(shape)
                .unwrap()
                .keys()
                .map(|key| format!("{}:{child}.{key}", serde_json::to_string(key).unwrap()))
                .collect();
            fields.push(format!("\"parent_title\":{parent}.title"));
            format!("{parent_source}.map(lambda {parent}: {{\"id\":{parent}.id,\"children\":{child_source}.map(lambda {child}: {{{}}}, max_parents={bound})}}, max_parents={bound})",fields.join(","))
        }
    }
}

/// Strictly smaller, same-output-type candidates. Runtime failure class is
/// preserved by the driver, not guessed by the shrinker.
pub fn smaller(expr: &Expr, source: &Shape) -> Vec<Expr> {
    let mut candidates = Vec::new();
    match expr {
        Expr::Source | Expr::ObservedSource => {}
        Expr::Project(e, keys) => {
            candidates.push(*e.clone());
            candidates.extend(
                smaller(e, source)
                    .into_iter()
                    .map(|e| Expr::Project(Box::new(e), keys.clone())),
            );
        }
        Expr::Take(e, n) => {
            candidates.push(*e.clone());
            candidates.extend(
                smaller(e, source)
                    .into_iter()
                    .map(|e| Expr::Take(Box::new(e), *n)),
            );
        }
        Expr::OrderId(e, desc) => {
            candidates.push(*e.clone());
            candidates.extend(
                smaller(e, source)
                    .into_iter()
                    .map(|e| Expr::OrderId(Box::new(e), *desc)),
            );
        }
        Expr::KeepTitle(e, title) => {
            candidates.push(*e.clone());
            candidates.extend(
                smaller(e, source)
                    .into_iter()
                    .map(|e| Expr::KeepTitle(Box::new(e), title.clone())),
            );
        }
        Expr::Record(e) => {
            candidates.push(*e.clone());
            candidates.extend(
                smaller(e, source)
                    .into_iter()
                    .map(|e| Expr::Record(Box::new(e))),
            );
        }
        Expr::Union(a, b) => {
            candidates.extend([*a.clone(), *b.clone()]);
            candidates.extend(
                smaller(a, source)
                    .into_iter()
                    .map(|a| Expr::Union(Box::new(a), b.clone())),
            );
            candidates.extend(
                smaller(b, source)
                    .into_iter()
                    .map(|b| Expr::Union(a.clone(), Box::new(b))),
            );
        }
        Expr::Nest {
            parents,
            children,
            bound,
            capture,
        } => {
            candidates.extend(smaller(parents, source).into_iter().map(|p| Expr::Nest {
                parents: Box::new(p),
                children: children.clone(),
                bound: *bound,
                capture: *capture,
            }));
            candidates.extend(smaller(children, source).into_iter().map(|c| Expr::Nest {
                parents: parents.clone(),
                children: Box::new(c),
                bound: *bound,
                capture: *capture,
            }));
        }
    }
    candidates
        .into_iter()
        .filter(|e| e.size() < expr.size() && e.shape(source).ok() == expr.shape(source).ok())
        .collect()
}

pub fn observed_cases() -> Vec<Expr> {
    vec![
        Expr::ObservedSource,
        Expr::Take(Box::new(Expr::ObservedSource), 1),
        Expr::OrderId(Box::new(Expr::ObservedSource), true),
        Expr::Union(
            Box::new(Expr::ObservedSource),
            Box::new(Expr::ObservedSource),
        ),
        Expr::Nest {
            parents: Box::new(Expr::ObservedSource),
            children: Box::new(Expr::ObservedSource),
            bound: 8,
            capture: false,
        },
        Expr::Nest {
            parents: Box::new(Expr::ObservedSource),
            children: Box::new(Expr::KeepTitle(
                Box::new(Expr::ObservedSource),
                "missing".into(),
            )),
            bound: 8,
            capture: false,
        },
    ]
}
