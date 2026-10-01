use super::*;
use model::Error;

#[test]
fn reference_boundary_preserves_missing_null_empty_and_continuations() {
    for size in [0, 1, 3] {
        for coverage in [Coverage::Complete, Coverage::Partial, Coverage::Unknown] {
            for continuation in [false, true] {
                let mut rows = source_rows(size);
                rows.coverage = coverage;
                rows.continuation = continuation;
                let expected = if coverage == Coverage::Complete && !continuation {
                    Ok(())
                } else {
                    Err(Error::Incomplete)
                };
                assert_eq!(
                    rows.require_materializable(),
                    expected,
                    "{size} {coverage:?} {continuation}"
                );
            }
        }
    }
    let mut rows = source_rows(1);
    rows.values[0].insert("score".into(), Value::Null);
    assert_eq!(rows.require_materializable(), Ok(()));
    rows.values[0].remove("score");
    assert_eq!(
        rows.require_materializable(),
        Err(Error::MissingField("score".into()))
    );
    assert_eq!(
        Expr::KeepTitle(Box::new(Expr::Source), "missing".into()).eval(&rows),
        Err(Error::MissingField("score".into()))
    );
    rows.values[0].insert("score".into(), json!("10"));
    assert_eq!(
        rows.require_materializable(),
        Err(Error::WrongType("record".into()))
    );
}

#[test]
fn reference_union_is_first_wins_set_union_on_declared_columns() {
    let source = source_rows(3);
    let titles = Expr::Project(Box::new(Expr::Source), vec!["title".into()]);
    let result = Expr::Union(Box::new(titles.clone()), Box::new(titles))
        .eval(&source)
        .unwrap();
    assert_eq!(
        serde_json::to_value(result.values).unwrap(),
        json!([{"title":"Alpha"},{"title":"Beta\n雪"}])
    );
    assert!(
        include_str!("../../../../plasm-core/src/prompt_render/assets/python-plasm-dag.txt")
            .contains("union: deduplicate, first wins, left then right")
    );
}

#[test]
fn reference_nesting_preserves_empty_children_and_parent_captures() {
    let source = source_rows(3);
    let empty = Expr::KeepTitle(Box::new(Expr::Source), "missing".into());
    let expr = Expr::Nest {
        parents: Box::new(Expr::Source),
        children: Box::new(empty),
        bound: 8,
        capture: true,
    };
    assert_eq!(
        serde_json::to_value(expr.eval(&source).unwrap().values).unwrap(),
        json!([
            {"id":"i1","children":[]},{"id":"i2","children":[]},{"id":"i3","children":[]}
        ])
    );
    let expr = Expr::Nest {
        parents: Box::new(Expr::Source),
        children: Box::new(Expr::Take(Box::new(Expr::Source), 1)),
        bound: 8,
        capture: true,
    };
    let result = expr.eval(&source).unwrap();
    assert_eq!(result.values[1]["children"][0]["parent_title"], "Beta\n雪");
    assert_eq!(result.values[1]["children"][0]["title"], "Alpha");
    let bounded = Expr::Nest {
        parents: Box::new(Expr::Source),
        children: Box::new(Expr::Source),
        bound: 2,
        capture: true,
    };
    assert_eq!(bounded.eval(&source), Err(Error::Bound));
}

#[test]
fn typed_grammar_and_shrink_steps_preserve_shape_and_decrease_size() {
    let source = source_rows(3);
    for expression in corpus::cases(&source.shape) {
        expression
            .eval(&source)
            .unwrap()
            .require_materializable()
            .unwrap();
        for smaller in corpus::smaller(&expression, &source.shape) {
            assert!(smaller.size() < expression.size());
            assert_eq!(
                smaller.shape(&source.shape),
                expression.shape(&source.shape)
            );
        }
    }
    let missing = Expr::Record(Box::new(Expr::Project(
        Box::new(Expr::Source),
        vec!["id".into()],
    )));
    assert_eq!(
        missing.shape(&source.shape),
        Err(Error::UnknownField("title".into()))
    );
    let incompatible = Expr::Union(
        Box::new(Expr::Source),
        Box::new(Expr::Project(Box::new(Expr::Source), vec!["id".into()])),
    );
    assert_eq!(
        incompatible.shape(&source.shape),
        Err(Error::IncompatibleUnion)
    );
}
