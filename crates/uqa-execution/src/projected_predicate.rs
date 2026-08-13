//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar predicates compiled against positional storage projections.

use uqa_core::Value;
use uqa_sql::ast::BinaryOp;
use uqa_sql::expr::IntegerWidth;
use uqa_sql::{SQLError, SQLParam};

use crate::ScalarExpr;

mod compile;
mod evaluate;

/// A predicate whose column lookups are resolved once to projection slots.
/// Unsupported expressions return `None` and continue through the canonical
/// map-backed evaluator.
pub struct ProjectedPredicate {
    expression: ProjectedExpr,
}

pub(super) enum ProjectedExpr {
    Field(usize),
    Literal(Value),
    Binary {
        op: BinaryOp,
        lhs: Box<Self>,
        rhs: Box<Self>,
        integer_width: Option<IntegerWidth>,
    },
    IntFieldComparison {
        field: usize,
        op: BinaryOp,
        literal: i64,
        field_on_left: bool,
    },
    Not(Box<Self>),
    And(Vec<Self>),
    Or(Vec<Self>),
    IsNull {
        expression: Box<Self>,
        negated: bool,
    },
    Between {
        expression: Box<Self>,
        low: Box<Self>,
        high: Box<Self>,
    },
    IntFieldBetween {
        field: usize,
        low: i64,
        high: i64,
    },
    InList {
        expression: Box<Self>,
        list: Vec<Self>,
        negated: bool,
    },
    Like {
        expression: Box<Self>,
        pattern: uqa_sql::expr::CompiledLikePattern,
    },
    Cast {
        expression: Box<Self>,
        ty: String,
    },
}

impl ProjectedPredicate {
    pub fn compile(
        expression: &ScalarExpr,
        fields: &[String],
        params: &[SQLParam],
    ) -> Result<Option<Self>, SQLError> {
        match compile::compile(expression, fields, params) {
            Ok(expression) => Ok(expression.map(|expression| Self { expression })),
            Err(SQLError::Unsupported(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn keep(&self, values: &[&Value]) -> Result<bool, SQLError> {
        evaluate::keep(&self.expression, values)
    }

    /// Evaluate directly against a composite physical row. Column names and
    /// qualifiers were resolved to logical positions during compilation, so
    /// the hot path neither builds a named row nor allocates a reference list.
    pub fn keep_row(&self, row: &crate::PhysicalRowView<'_>) -> Result<bool, SQLError> {
        evaluate::keep_row(&self.expression, row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{eval_scalar, ScalarEvalContext};
    use uqa_sql::expr::truthy;

    #[test]
    fn positional_predicate_preserves_null_and_short_circuit_semantics() {
        let expression = ScalarExpr::And(vec![
            ScalarExpr::Between {
                expr: Box::new(ScalarExpr::Column("x".into())),
                low: Box::new(ScalarExpr::Literal(Value::Int(2))),
                high: Box::new(ScalarExpr::Literal(Value::Int(4))),
            },
            ScalarExpr::IsNull {
                expr: Box::new(ScalarExpr::Column("y".into())),
                negated: false,
            },
        ]);
        let predicate = ProjectedPredicate::compile(&expression, &["x".into(), "y".into()], &[])
            .unwrap()
            .unwrap();
        assert!(predicate.keep(&[&Value::Int(3), &Value::Null]).unwrap());
        assert!(!predicate.keep(&[&Value::Int(5), &Value::Null]).unwrap());
        assert!(!predicate.keep(&[&Value::Null, &Value::Null]).unwrap());
    }

    #[test]
    fn projected_q1_and_q6_predicates_match_the_canonical_evaluator() {
        let fields = vec!["discount".into(), "quantity".into(), "ship_day".into()];
        let q6 = ScalarExpr::And(vec![
            ScalarExpr::Between {
                expr: Box::new(ScalarExpr::Column("ship_day".into())),
                low: Box::new(ScalarExpr::Param(1)),
                high: Box::new(ScalarExpr::Literal(Value::Int(2_190))),
            },
            ScalarExpr::Between {
                expr: Box::new(ScalarExpr::Column("discount".into())),
                low: Box::new(ScalarExpr::Literal(Value::Int(2))),
                high: Box::new(ScalarExpr::Literal(Value::Int(8))),
            },
            ScalarExpr::Binary {
                op: BinaryOp::Greater,
                lhs: Box::new(ScalarExpr::Literal(Value::Int(40))),
                rhs: Box::new(ScalarExpr::Column("quantity".into())),
            },
        ]);
        let params = vec![SQLParam::Scalar(Value::Int(365))];
        let predicate = ProjectedPredicate::compile(&q6, &fields, &params)
            .unwrap()
            .unwrap();

        let discounts = [Value::Null, Value::Int(1), Value::Int(2), Value::Int(8)];
        let quantities = [
            Value::Null,
            Value::Int(39),
            Value::Int(40),
            Value::Float(39.5),
        ];
        let ship_days = [
            Value::Null,
            Value::Int(364),
            Value::Int(365),
            Value::Int(2_190),
            Value::Int(2_191),
        ];
        for discount in &discounts {
            for quantity in &quantities {
                for ship_day in &ship_days {
                    assert_projected_parity(
                        &q6,
                        &predicate,
                        &fields,
                        &[discount.clone(), quantity.clone(), ship_day.clone()],
                        &params,
                    );
                }
            }
        }

        let q1 = ScalarExpr::Binary {
            op: BinaryOp::LessEqual,
            lhs: Box::new(ScalarExpr::Column("ship_day".into())),
            rhs: Box::new(ScalarExpr::Literal(Value::Int(2_449))),
        };
        let predicate = ProjectedPredicate::compile(&q1, &fields, &[])
            .unwrap()
            .unwrap();
        for ship_day in [Value::Null, Value::Int(2_449), Value::Int(2_450)] {
            assert_projected_parity(
                &q1,
                &predicate,
                &fields,
                &[Value::Int(0), Value::Int(0), ship_day],
                &[],
            );
        }
    }

    #[test]
    fn projected_like_predicates_match_the_canonical_evaluator() {
        for (name, pattern) in [
            ("like", "%"),
            ("like", "%green%"),
            ("like", "%special%requests%"),
            ("like", "a_c"),
            ("ilike", "%GREEN%"),
        ] {
            let expression = ScalarExpr::Func {
                name: name.into(),
                binding: None,
                args: vec![
                    ScalarExpr::Column("text".into()),
                    ScalarExpr::Literal(Value::Str(pattern.into())),
                ],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            let fields = vec!["text".into()];
            let predicate = ProjectedPredicate::compile(&expression, &fields, &[])
                .unwrap()
                .unwrap();
            for value in [
                Value::Str("forest green part".into()),
                Value::FixedChar("GREEN   ".into()),
                Value::Str("a-c".into()),
                Value::Str("special pending requests".into()),
                Value::Null,
            ] {
                assert_projected_parity(&expression, &predicate, &fields, &[value], &[]);
            }
        }
    }

    #[test]
    fn qualified_like_runs_directly_on_a_composite_physical_row() {
        let expression = ScalarExpr::Not(Box::new(ScalarExpr::Func {
            name: "like".into(),
            binding: None,
            args: vec![
                ScalarExpr::qualified_column("o", "comment"),
                ScalarExpr::Literal(Value::Str("%special%requests%".into())),
            ],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        }));
        let left_schema = crate::RowSchema::new(vec!["c.id".into()]);
        let right_schema = crate::RowSchema::new(vec!["o.comment".into()]);
        let schema = crate::RowSchema::join(&left_schema, &right_schema, std::iter::empty());
        let predicate = ProjectedPredicate::compile(&expression, schema.columns(), &[])
            .unwrap()
            .unwrap();

        let accepted = crate::PhysicalRow::concat(
            &crate::PhysicalRow::from_values(vec![Value::Int(1)]),
            &crate::PhysicalRow::from_values(vec![Value::Str("ordinary order".into())]),
        );
        let rejected = crate::PhysicalRow::concat(
            &crate::PhysicalRow::from_values(vec![Value::Int(1)]),
            &crate::PhysicalRow::from_values(vec![Value::Str("special pending requests".into())]),
        );
        assert!(predicate.keep_row(&schema.view(&accepted)).unwrap());
        assert!(!predicate.keep_row(&schema.view(&rejected)).unwrap());
    }

    #[test]
    fn projected_predicate_folds_typed_literals_once() {
        let expression = ScalarExpr::Binary {
            op: BinaryOp::Less,
            lhs: Box::new(ScalarExpr::Column("day".into())),
            rhs: Box::new(ScalarExpr::Cast {
                expr: Box::new(ScalarExpr::Literal(Value::Str("1995-03-15".into()))),
                ty: "date".into(),
            }),
        };
        let predicate = ProjectedPredicate::compile(&expression, &["day".into()], &[])
            .unwrap()
            .unwrap();

        let ProjectedExpr::Binary { rhs, .. } = &predicate.expression else {
            panic!("expected a compiled comparison");
        };
        assert!(matches!(
            rhs.as_ref(),
            ProjectedExpr::Literal(Value::Temporal(_))
        ));
    }

    fn assert_projected_parity(
        expression: &ScalarExpr,
        predicate: &ProjectedPredicate,
        fields: &[String],
        values: &[Value],
        params: &[SQLParam],
    ) {
        let row = fields
            .iter()
            .cloned()
            .zip(values.iter().cloned())
            .collect::<uqa_sql::ResultRow>();
        let expected = eval_scalar(expression, &ScalarEvalContext::new(Some(&row), params))
            .map(|value| truthy(&value));
        let references = values.iter().collect::<Vec<_>>();
        let actual = predicate.keep(&references);
        match (expected, actual) {
            (Ok(expected), Ok(actual)) => assert_eq!(actual, expected, "row: {row:?}"),
            (Err(expected), Err(actual)) => assert_eq!(actual.to_string(), expected.to_string()),
            (expected, actual) => panic!("projected result {actual:?} != canonical {expected:?}"),
        }
    }
}
