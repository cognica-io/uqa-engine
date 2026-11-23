//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ColumnType, RowSchema};

fn bound(sql: &str) -> ScalarExpr {
    let crate::Statement::Select(mut query) =
        crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
    else {
        panic!("SELECT")
    };
    let scalar = crate::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar;
    crate::bind_type_introspection(
        scalar,
        &RowSchema::with_types(vec!["c".into()], vec![Some(ColumnType::Integer)]),
        &[],
    )
}

fn lower(sql: &str) -> Option<RetrievalExpr> {
    struct Source;
    impl GraphNameCatalog for Source {
        fn list_graphs(&self) -> Result<Vec<String>, SQLError> {
            unreachable!("membership reads no graphs")
        }
    }
    impl RetrievalArguments for Source {
        fn evaluate_argument(&self, _: &ScalarExpr, _: &[SQLParam]) -> Result<Value, SQLError> {
            unreachable!("membership uses the constant evaluator")
        }
    }
    lower_where_bound(
        &Source,
        &bound(sql),
        &RetrievalConstants {
            params: &[],
            evaluate: &|expression, _| constant(expression),
            stores: &|_| true,
        },
    )
    .unwrap()
}

fn constant(expression: &ScalarExpr) -> Result<Value, SQLError> {
    match expression {
        ScalarExpr::Literal(value) | ScalarExpr::TypedLiteral { value, .. } => Ok(value.clone()),
        ScalarExpr::Cast { expr, ty, .. } => crate::expr::cast_value(&constant(expr)?, ty),
        _ => Err(SQLError::Internal(
            "test accepts only retained constants".into(),
        )),
    }
}

#[test]
fn analyzed_membership_keeps_set_filter_and_null_rejection() {
    let Some(RetrievalExpr::Filter {
        field,
        predicate: Predicate::InSet(values),
        ..
    }) = lower("c IN (1, 2, NULL)")
    else {
        panic!("set filter")
    };
    assert_eq!(field, "c");
    assert_eq!(values, BTreeSet::from([Value::Int(1), Value::Int(2)]));
    assert!(matches!(
        lower("c NOT IN (1, NULL)"),
        Some(RetrievalExpr::Empty)
    ));
    let Some(RetrievalExpr::Intersect(parts)) = lower("c NOT IN (1, 2)") else {
        panic!("three-valued negation")
    };
    assert!(matches!(
        parts.as_slice(),
        [
            RetrievalExpr::Complement(_),
            RetrievalExpr::Filter {
                predicate: Predicate::IsNotNull,
                ..
            }
        ]
    ));
}

#[test]
fn temporal_membership_retains_sql_operators_instead_of_physical_set_keys() {
    for value in [
        uqa_core::TemporalValue::Date { days: 18_262 },
        uqa_core::TemporalValue::Timestamp {
            postgres_micros: 1_577_836_800_000_000,
        },
        uqa_core::TemporalValue::TimestampTz {
            postgres_micros: 1_577_836_800_000_000,
        },
    ] {
        for negated in [false, true] {
            let scalar = ScalarExpr::InList {
                expr: Box::new(ScalarExpr::Column("d".into())),
                list: vec![ScalarExpr::Literal(Value::Temporal(value.clone()))],
                negated,
            };
            let constants = RetrievalConstants {
                params: &[],
                evaluate: &|expression, _| constant(expression),
                stores: &|_| true,
            };
            assert!(lower_where(&scalar, &constants).is_none());
            let mut folded = bound(if negated {
                "c NOT IN (1, 2)"
            } else {
                "c IN (1, 2)"
            });
            let ScalarExpr::Func { args, .. } = &mut folded else {
                panic!("analyzed scalar-array comparison");
            };
            args[1] = ScalarExpr::Literal(Value::Array(
                uqa_core::ArrayValue::try_new(vec![Value::Temporal(value.clone())]).unwrap(),
            ));
            assert!(lower_where(&folded, &constants).is_none());
            let ScalarExpr::Func { args, .. } = &mut folded else {
                unreachable!()
            };
            let ScalarExpr::Literal(value) = &args[1] else {
                unreachable!()
            };
            args[1] = resident(value);
            assert!(lower_where(&folded, &constants).is_none());
        }
    }
}

fn resident(value: &Value) -> ScalarExpr {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    ScalarExpr::RetainedLiteral(crate::ir::RetainedScalarLiteral::new(
        crate::ir::RetainedConstantValue::from_produced(
            control.copy_value(value).unwrap(),
            &control,
        )
        .unwrap(),
        None,
        None,
        None,
    ))
}

#[test]
fn retained_membership_preserves_retrieval_sets_and_null_rejection() {
    let constants = RetrievalConstants {
        params: &[],
        evaluate: &|_, _| panic!("membership borrows the resident array"),
        stores: &|_| true,
    };
    for (negated, has_null) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut expression = bound(if negated {
            "c NOT IN (1, 2)"
        } else {
            "c IN (1, 2)"
        });
        let ScalarExpr::Func { args, .. } = &mut expression else {
            panic!("membership")
        };
        let mut values = vec![Value::Int(2), Value::Int(1), Value::Int(2)];
        if has_null {
            values.push(Value::Null);
        }
        args[1] = resident(&Value::Array(
            uqa_core::ArrayValue::try_new(values).unwrap(),
        ));
        let lowered = lower_where(&expression, &constants).unwrap();
        if !negated {
            let RetrievalExpr::Filter {
                field,
                predicate: Predicate::InSet(values),
                ..
            } = lowered
            else {
                panic!("set filter")
            };
            assert_eq!(field, "c");
            assert_eq!(values, BTreeSet::from([Value::Int(1), Value::Int(2)]));
        } else if has_null {
            assert!(matches!(lowered, RetrievalExpr::Empty));
        } else {
            let RetrievalExpr::Intersect(parts) = lowered else {
                panic!("negated set")
            };
            assert!(matches!(
                parts.as_slice(),
                [
                    RetrievalExpr::Complement(_),
                    RetrievalExpr::Filter {
                        predicate: Predicate::IsNotNull,
                        ..
                    }
                ]
            ));
        }
    }
}

#[test]
fn temporal_filters_retain_sql_comparison_instead_of_carrier_order() {
    let constants = RetrievalConstants {
        params: &[],
        evaluate: &|expression, _| constant(expression),
        stores: &|_| true,
    };
    let column = ScalarExpr::Column("d".into());
    for value in [
        uqa_core::TemporalValue::Date { days: 18_262 },
        uqa_core::TemporalValue::Timestamp {
            postgres_micros: 1_577_836_800_000_000,
        },
        uqa_core::TemporalValue::TimestampTz {
            postgres_micros: 1_577_836_800_000_000,
        },
    ] {
        let literal = ScalarExpr::Literal(Value::Temporal(value));
        for op in [
            BinaryOp::Equal,
            BinaryOp::NotEqual,
            BinaryOp::Less,
            BinaryOp::LessEqual,
            BinaryOp::Greater,
            BinaryOp::GreaterEqual,
        ] {
            for (lhs, rhs) in [(&column, &literal), (&literal, &column)] {
                assert!(lower_where(
                    &ScalarExpr::Binary {
                        op,
                        lhs: Box::new(lhs.clone()),
                        rhs: Box::new(rhs.clone())
                    },
                    &constants
                )
                .is_none());
            }
        }
        for (low, high) in [
            (&literal, &literal),
            (&literal, &ScalarExpr::Literal(Value::Int(0))),
            (&ScalarExpr::Literal(Value::Int(0)), &literal),
        ] {
            let bounds = [
                ScalarExpr::Binary {
                    op: BinaryOp::GreaterEqual,
                    lhs: Box::new(column.clone()),
                    rhs: Box::new(low.clone()),
                },
                ScalarExpr::Binary {
                    op: BinaryOp::LessEqual,
                    lhs: Box::new(column.clone()),
                    rhs: Box::new(high.clone()),
                },
            ];
            assert!(range(&bounds, &constants).is_none());
            assert!(lower_where(&ScalarExpr::And(bounds.to_vec()), &constants).is_none());
            assert!(lower_where(
                &ScalarExpr::Between {
                    expr: Box::new(column.clone()),
                    low: Box::new(low.clone()),
                    high: Box::new(high.clone())
                },
                &constants
            )
            .is_none());
        }
    }
    assert!(lower("c = 1").is_some());
    assert!(lower("c BETWEEN 1 AND 3").is_some());
    assert!(lower("c >= 1 AND c <= 3").is_some());
}

#[test]
fn set_recognition_retains_conversions_and_quantified_operator_identity() {
    for sql in [
        "c < ANY(ARRAY[1, 2])",
        "c = ALL(ARRAY[1, 2])",
        "c <> ALL(ARRAY[]::integer[])",
        "c IN (c, 2)",
    ] {
        assert!(
            crate::semantics::membership_operands(&bound(sql)).is_none(),
            "{sql}"
        );
    }
    let mut scalar = bound("c IN (1, 2)");
    let ScalarExpr::Func { args, .. } = &mut scalar else {
        panic!("scalar-array comparison")
    };
    args[1] = ScalarExpr::Cast {
        expr: Box::new(args[1].clone()),
        ty: "bigint[]".into(),
        implicit: true,
    };
    assert!(crate::semantics::membership_operands(&scalar).is_none());
    scalar = bound("c IN (1, 2)");
    let ScalarExpr::Func { binding, .. } = &mut scalar else {
        unreachable!()
    };
    *binding = None;
    assert!(crate::semantics::membership_operands(&scalar).is_none());
}

#[test]
fn analyzed_range_keeps_one_access_path_only_for_the_same_uncoerced_column() {
    for sql in ["c BETWEEN 1 AND 3", "c >= 1 AND c <= 3"] {
        let Some(RetrievalExpr::Filter {
            field,
            predicate: Predicate::Between { low, high },
            source: None,
        }) = lower(sql)
        else {
            panic!("one range access path: {sql}");
        };
        assert_eq!(field, "c");
        assert_eq!(low, Value::Int(1));
        assert_eq!(high, Value::Int(3));
    }
    let constants = RetrievalConstants {
        params: &[],
        evaluate: &|expression, _| constant(expression),
        stores: &|_| true,
    };
    let qualified = |name: &str| ScalarExpr::QualifiedColumn {
        qualifier: name.into(),
        column: "c".into(),
    };
    for upper in [
        qualified("b"),
        ScalarExpr::Cast {
            expr: Box::new(qualified("a")),
            ty: "text".into(),
            implicit: false,
        },
    ] {
        let parts = [
            ScalarExpr::Binary {
                op: BinaryOp::GreaterEqual,
                lhs: Box::new(qualified("a")),
                rhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
            },
            ScalarExpr::Binary {
                op: BinaryOp::LessEqual,
                lhs: Box::new(upper),
                rhs: Box::new(ScalarExpr::Literal(Value::Int(3))),
            },
        ];
        assert!(range(&parts, &constants).is_none());
    }
}

/// The range that two bounds form when they are a whole conjunction.
fn range(parts: &[ScalarExpr], constants: &RetrievalConstants<'_>) -> Option<RetrievalExpr> {
    match predicates::conjuncts(parts, constants).as_slice() {
        [Conjunct::Range(range)] => Some(range.clone()),
        _ => None,
    }
}

fn filter(expression: &RetrievalExpr) -> (&str, &Predicate) {
    let RetrievalExpr::Filter {
        field,
        predicate,
        source: None,
    } = expression
    else {
        panic!("a filter of the whole relation: {expression:?}");
    };
    (field, predicate)
}

#[test]
fn two_bounds_of_one_column_are_one_range_whatever_their_strictness_and_order() {
    use uqa_core::RangeEnd::{Exclusive, Inclusive};
    let (one, three) = (Value::Int(1), Value::Int(3));
    for (sql, low, high) in [
        (
            "c > 1 AND c <= 3",
            Exclusive(one.clone()),
            Inclusive(three.clone()),
        ),
        (
            "c >= 1 AND c < 3",
            Inclusive(one.clone()),
            Exclusive(three.clone()),
        ),
        (
            "c > 1 AND c < 3",
            Exclusive(one.clone()),
            Exclusive(three.clone()),
        ),
        (
            "c <= 3 AND c > 1",
            Exclusive(one.clone()),
            Inclusive(three.clone()),
        ),
        (
            "1 < c AND 3 >= c",
            Exclusive(one.clone()),
            Inclusive(three.clone()),
        ),
        (
            "3 > c AND c >= 1",
            Inclusive(one.clone()),
            Exclusive(three.clone()),
        ),
    ] {
        let lowered = lower(sql).unwrap_or_else(|| panic!("one range access path: {sql}"));
        assert_eq!(
            filter(&lowered),
            ("c", &Predicate::Range { low, high }),
            "{sql}"
        );
    }
    // The pair that includes both ends stays the range it always was, in either order of its bounds and of their operands.
    let between = format!("{:?}", lower("c BETWEEN 1 AND 3").unwrap());
    for sql in [
        "c >= 1 AND c <= 3",
        "c <= 3 AND c >= 1",
        "1 <= c AND 3 >= c",
    ] {
        assert_eq!(format!("{:?}", lower(sql).unwrap()), between, "{sql}");
    }
}

#[test]
fn a_range_forms_beside_the_other_predicates_of_a_conjunction() {
    use uqa_core::RangeEnd::Exclusive;
    let children = |sql: &str| {
        let Some(RetrievalExpr::Intersect(children)) = lower(sql) else {
            panic!("a conjunction: {sql}");
        };
        children
    };
    let range = |low: i64, high: i64| Predicate::Range {
        low: Exclusive(Value::Int(low)),
        high: Exclusive(Value::Int(high)),
    };

    // The range stands where the first of its two bounds stood.
    let lowered = children("c <> 2 AND c > 1 AND c <> 4 AND c < 9");
    assert_eq!(
        lowered.iter().map(filter).collect::<Vec<_>>(),
        [
            ("c", &Predicate::NotEquals(Value::Int(2))),
            ("c", &range(1, 9)),
            ("c", &Predicate::NotEquals(Value::Int(4))),
        ]
    );

    // A further bound of a column whose range is formed keeps its comparison.
    let lowered = children("c > 1 AND c < 9 AND c < 3 AND c > 2");
    assert_eq!(
        lowered.iter().map(filter).collect::<Vec<_>>(),
        [("c", &range(1, 9)), ("c", &range(2, 3))]
    );
    let lowered = children("c > 1 AND c < 9 AND c < 3");
    assert_eq!(
        lowered.iter().map(filter).collect::<Vec<_>>(),
        [
            ("c", &range(1, 9)),
            ("c", &Predicate::LessThan(Value::Int(3))),
        ]
    );

    // Two bounds on the same side are no range.
    let lowered = children("c > 1 AND c >= 2");
    assert_eq!(
        lowered.iter().map(filter).collect::<Vec<_>>(),
        [
            ("c", &Predicate::GreaterThan(Value::Int(1))),
            ("c", &Predicate::GreaterThanOrEqual(Value::Int(2))),
        ]
    );
}

#[test]
fn a_bound_that_cannot_be_an_end_leaves_the_next_one_to_form_the_range() {
    use uqa_core::RangeEnd::Exclusive;
    let constants = RetrievalConstants {
        params: &[],
        evaluate: &|expression, _| constant(expression),
        stores: &|_| true,
    };
    let column = ScalarExpr::Column("c".into());
    let comparison = |op, rhs: ScalarExpr| ScalarExpr::Binary {
        op,
        lhs: Box::new(column.clone()),
        rhs: Box::new(rhs),
    };
    // `c < other` compares two columns and is no constant end.
    let parts = [
        comparison(BinaryOp::Greater, ScalarExpr::Literal(Value::Int(1))),
        comparison(BinaryOp::Less, ScalarExpr::Column("other".into())),
        comparison(BinaryOp::Less, ScalarExpr::Literal(Value::Int(9))),
    ];
    let conjuncts = predicates::conjuncts(&parts, &constants);
    let [Conjunct::Range(formed), Conjunct::Part(left)] = conjuncts.as_slice() else {
        panic!("a range and the comparison of two columns");
    };
    assert_eq!(
        filter(formed),
        (
            "c",
            &Predicate::Range {
                low: Exclusive(Value::Int(1)),
                high: Exclusive(Value::Int(9)),
            }
        )
    );
    assert_eq!(*left, &parts[1]);
}
