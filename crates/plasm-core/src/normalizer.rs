use crate::{NormalizationError, Predicate};

const MAX_DEPTH: usize = 50; // Prevent stack overflow on deeply nested predicates

/// Normalize a predicate to canonical form.
///
/// This performs:
/// - Flattening of nested And/Or
/// - DeMorgan's law for Not
/// - Elimination of trivial True/False
/// - Deduplication of identical predicates
pub fn normalize(predicate: Predicate) -> Result<Predicate, NormalizationError> {
    if predicate.depth() > MAX_DEPTH {
        return Err(NormalizationError::MaxDepthExceeded);
    }

    let normalized = normalize_iterative(&predicate);
    Ok(normalized)
}

#[derive(Clone, Copy)]
enum Junction {
    And,
    Or,
}

fn normalize_iterative(root: &Predicate) -> Predicate {
    enum Work<'a> {
        Visit(&'a Predicate, bool),
        Join(Junction, usize),
        Relation(&'a str, bool, bool),
    }
    let mut pending = vec![Work::Visit(root, false)];
    let mut values = Vec::new();
    while let Some(step) = pending.pop() {
        match step {
            Work::Visit(predicate, negate) => match predicate {
                Predicate::And { args } | Predicate::Or { args } => {
                    let and = matches!(predicate, Predicate::And { .. }) != negate;
                    pending.push(Work::Join(
                        if and { Junction::And } else { Junction::Or },
                        args.len(),
                    ));
                    pending.extend(args.iter().rev().map(|child| Work::Visit(child, negate)));
                }
                Predicate::Not { predicate } => pending.push(Work::Visit(predicate, !negate)),
                Predicate::ExistsRelation {
                    relation,
                    predicate,
                } => {
                    pending.push(Work::Relation(relation, predicate.is_some(), negate));
                    if let Some(child) = predicate {
                        pending.push(Work::Visit(child, false));
                    }
                }
                Predicate::True => values.push(if negate {
                    Predicate::False
                } else {
                    Predicate::True
                }),
                Predicate::False => values.push(if negate {
                    Predicate::True
                } else {
                    Predicate::False
                }),
                Predicate::Comparison { .. } => {
                    let leaf = predicate.clone();
                    values.push(if negate {
                        Predicate::Not {
                            predicate: Box::new(leaf),
                        }
                    } else {
                        leaf
                    });
                }
            },
            Work::Join(kind, count) => {
                let args = values.split_off(values.len() - count);
                values.push(normalize_junction(kind, args));
            }
            Work::Relation(relation, child, negate) => {
                let predicate =
                    child.then(|| Box::new(values.pop().expect("normalized relation child")));
                let relation = Predicate::ExistsRelation {
                    relation: relation.to_owned(),
                    predicate,
                };
                values.push(if negate {
                    Predicate::Not {
                        predicate: Box::new(relation),
                    }
                } else {
                    relation
                });
            }
        }
    }
    values.pop().expect("normalized root")
}

/// Combine already-normalized children; never reenter the normalization driver.
fn normalize_junction(kind: Junction, args: Vec<Predicate>) -> Predicate {
    let mut normalized_args = Vec::new();
    for mut normalized in args {
        match (kind, &mut normalized) {
            (Junction::And, Predicate::And { args }) | (Junction::Or, Predicate::Or { args }) => {
                normalized_args.extend(std::mem::take(args));
            }
            (Junction::And, Predicate::False) => return Predicate::False,
            (Junction::Or, Predicate::True) => return Predicate::True,
            (Junction::And, Predicate::True) | (Junction::Or, Predicate::False) => {}
            _ => normalized_args.push(normalized),
        }
    }
    normalized_args.sort_by_cached_key(|p| serde_json::to_string(p).unwrap_or_default());
    normalized_args.dedup();
    if normalized_args.len() == 1 {
        return normalized_args.pop().unwrap();
    }
    match kind {
        Junction::And if normalized_args.is_empty() => Predicate::True,
        Junction::Or if normalized_args.is_empty() => Predicate::False,
        Junction::And => Predicate::And {
            args: normalized_args,
        },
        Junction::Or => Predicate::Or {
            args: normalized_args,
        },
    }
}

/// Check normalized structure using an explicit worklist.
pub fn is_normalized(predicate: &Predicate) -> bool {
    let mut pending = vec![predicate];
    while let Some(node) = pending.pop() {
        match node {
            Predicate::True | Predicate::False | Predicate::Comparison { .. } => {}
            Predicate::And { args } | Predicate::Or { args } => {
                let and = matches!(node, Predicate::And { .. });
                if args.iter().any(|child| {
                    child.is_trivial()
                        || (and && matches!(child, Predicate::And { .. }))
                        || (!and && matches!(child, Predicate::Or { .. }))
                }) || !is_sorted_and_deduped(args)
                {
                    return false;
                }
                pending.extend(args.iter().rev());
            }
            Predicate::Not { predicate } => {
                if matches!(
                    predicate.as_ref(),
                    Predicate::Not { .. }
                        | Predicate::And { .. }
                        | Predicate::Or { .. }
                        | Predicate::True
                        | Predicate::False
                ) {
                    return false;
                }
                pending.push(predicate);
            }
            Predicate::ExistsRelation { predicate, .. } => {
                if let Some(child) = predicate {
                    pending.push(child);
                }
            }
        }
    }
    true
}

fn is_sorted_and_deduped(args: &[Predicate]) -> bool {
    let serialized: Vec<String> = args
        .iter()
        .map(|p| serde_json::to_string(p).unwrap_or_default())
        .collect();
    let mut sorted = serialized.clone();
    sorted.sort();
    sorted.dedup();
    serialized == sorted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CompOp;

    #[test]
    fn test_flatten_and() {
        let predicate = Predicate::And {
            args: vec![
                Predicate::comparison("a", CompOp::Eq, "1"),
                Predicate::And {
                    args: vec![
                        Predicate::comparison("b", CompOp::Eq, "2"),
                        Predicate::comparison("c", CompOp::Eq, "3"),
                    ],
                },
            ],
        };

        let normalized = normalize(predicate).unwrap();

        if let Predicate::And { args } = &normalized {
            assert_eq!(args.len(), 3);
            // All three comparisons should be at the same level
        } else {
            panic!("Expected And predicate");
        }
    }

    #[test]
    fn test_eliminate_true_in_and() {
        let predicate = Predicate::And {
            args: vec![
                Predicate::comparison("a", CompOp::Eq, "1"),
                Predicate::True,
                Predicate::comparison("b", CompOp::Eq, "2"),
            ],
        };

        let normalized = normalize(predicate).unwrap();

        if let Predicate::And { args } = &normalized {
            assert_eq!(args.len(), 2);
            // True should be eliminated
        } else {
            panic!("Expected And predicate");
        }
    }

    #[test]
    fn test_false_in_and_makes_false() {
        let predicate = Predicate::And {
            args: vec![
                Predicate::comparison("a", CompOp::Eq, "1"),
                Predicate::False,
                Predicate::comparison("b", CompOp::Eq, "2"),
            ],
        };

        let normalized = normalize(predicate).unwrap();

        assert_eq!(normalized, Predicate::False);
    }

    #[test]
    fn test_demorgans_law() {
        let predicate = Predicate::Not {
            predicate: Box::new(Predicate::And {
                args: vec![
                    Predicate::comparison("a", CompOp::Eq, "1"),
                    Predicate::comparison("b", CompOp::Eq, "2"),
                ],
            }),
        };

        let normalized = normalize(predicate).unwrap();

        // Should become Or(Not(a=1), Not(b=2))
        if let Predicate::Or { args } = &normalized {
            assert_eq!(args.len(), 2);
            for arg in args {
                assert!(matches!(arg, Predicate::Not { .. }));
            }
        } else {
            panic!("Expected Or predicate after DeMorgan's law");
        }
    }

    #[test]
    fn test_double_negation() {
        let predicate = Predicate::Not {
            predicate: Box::new(Predicate::Not {
                predicate: Box::new(Predicate::comparison("a", CompOp::Eq, "1")),
            }),
        };

        let normalized = normalize(predicate).unwrap();

        // Should become just a=1
        assert!(matches!(normalized, Predicate::Comparison { .. }));
    }

    #[test]
    fn test_deduplication() {
        let predicate = Predicate::And {
            args: vec![
                Predicate::comparison("a", CompOp::Eq, "1"),
                Predicate::comparison("b", CompOp::Eq, "2"),
                Predicate::comparison("a", CompOp::Eq, "1"), // duplicate
            ],
        };

        let normalized = normalize(predicate).unwrap();

        if let Predicate::And { args } = &normalized {
            assert_eq!(args.len(), 2); // Duplicate should be removed
        } else {
            panic!("Expected And predicate");
        }
    }

    #[test]
    fn test_single_arg_and_becomes_arg() {
        let predicate = Predicate::And {
            args: vec![Predicate::comparison("a", CompOp::Eq, "1")],
        };

        let normalized = normalize(predicate).unwrap();

        assert!(matches!(normalized, Predicate::Comparison { .. }));
    }

    #[test]
    fn test_empty_and_becomes_true() {
        let predicate = Predicate::And { args: vec![] };

        let normalized = normalize(predicate).unwrap();

        assert_eq!(normalized, Predicate::True);
    }

    #[test]
    fn test_idempotency() {
        let predicate = Predicate::And {
            args: vec![
                Predicate::comparison("a", CompOp::Eq, "1"),
                Predicate::comparison("b", CompOp::Gt, 2.0),
            ],
        };

        let normalized1 = normalize(predicate.clone()).unwrap();
        let normalized2 = normalize(normalized1.clone()).unwrap();

        assert_eq!(normalized1, normalized2);
        assert!(is_normalized(&normalized2));
    }
}
