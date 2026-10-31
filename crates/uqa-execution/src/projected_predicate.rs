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

use crate::{RowSchema, ScalarExpr};

mod compile;
mod evaluate;
#[cfg(test)]
mod production_tests;

/// A predicate whose column lookups are resolved once to projection slots.
/// Unsupported expressions return `None` and continue through the canonical
/// map-backed evaluator.
pub struct ProjectedPredicate {
    expression: ProjectedExpr,
    _memory: Option<uqa_core::memory::MemoryReservation>,
}

#[derive(Clone, Copy)]
pub(super) enum ProjectedIntPredicate {
    Comparison {
        field: usize,
        op: BinaryOp,
        literal: i64,
        field_on_left: bool,
    },
    Between {
        field: usize,
        low: i64,
        high: i64,
    },
}

pub(super) enum ProjectedExpr {
    Field(usize),
    Literal(Value),
    Binary {
        op: BinaryOp,
        lhs: Box<Self>,
        rhs: Box<Self>,
        integer_width: Option<IntegerWidth>,
        real_arithmetic: bool,
    },
    UnaryMinus(Box<Self>),
    IntFieldComparison {
        field: usize,
        op: BinaryOp,
        literal: i64,
        field_on_left: bool,
    },
    Not(Box<Self>),
    And(Vec<Self>),
    IntFieldConjunction(Vec<ProjectedIntPredicate>),
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
        Self::compile_with_schema(expression, &RowSchema::new(fields.to_vec()), params)
    }

    /// Compile against structured SQL identities instead of interpreting punctuation in public labels as qualification metadata.
    pub fn compile_with_schema(
        expression: &ScalarExpr,
        schema: &RowSchema,
        params: &[SQLParam],
    ) -> Result<Option<Self>, SQLError> {
        Self::compile_with_schema_with_control(
            expression,
            schema,
            params,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
    }

    pub fn compile_with_schema_with_control(
        expression: &ScalarExpr,
        schema: &RowSchema,
        params: &[SQLParam],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<Self>, SQLError> {
        let expression = uqa_sql::bind_type_introspection_owned(
            expression.copy_for_rewrite(control)?,
            schema,
            params,
            None,
            control,
        )?;
        Self::compile_bound_with_control(&expression, schema, params, control)
    }

    pub(crate) fn compile_bound(
        expression: &ScalarExpr,
        schema: &RowSchema,
        params: &[SQLParam],
    ) -> Result<Option<Self>, SQLError> {
        Self::compile_bound_with_control(
            expression,
            schema,
            params,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
    }

    pub(crate) fn compile_bound_with_control(
        expression: &ScalarExpr,
        schema: &RowSchema,
        params: &[SQLParam],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<Self>, SQLError> {
        control.check()?;
        let mut local = true;
        expression.try_visit(&mut |node| {
            control.check()?;
            local &= match node {
                ScalarExpr::Column(name) => schema.unqualified_position(name).is_some(),
                ScalarExpr::QualifiedColumn { qualifier, column } => {
                    schema.qualified_position(qualifier, column).is_some()
                }
                ScalarExpr::InternalColumn(_)
                | ScalarExpr::ScalarSubquery(_)
                | ScalarExpr::Exists { .. }
                | ScalarExpr::InSubquery { .. } => false,
                _ => true,
            };
            Ok::<_, SQLError>(true)
        })?;
        // A projected predicate only owns local positional inputs. Expressions needing another row or a subquery belong to the caller's scoped evaluator, including their type binding.
        if !local {
            return Ok(None);
        }
        match compile::compile(expression, schema, params, control) {
            Ok(expression) => Ok(expression.map(|expression| {
                let (expression, memory) = expression.into_parts();
                Self {
                    expression,
                    _memory: memory,
                }
            })),
            Err(SQLError::Unsupported(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    #[inline]
    pub fn keep(&self, values: &[&Value]) -> Result<bool, SQLError> {
        evaluate::keep(&self.expression, values)
    }

    /// Evaluate against backend-owned values through a positional projection without constructing an intermediate reference array. A `usize::MAX` projection slot reads as SQL NULL.
    #[inline]
    pub fn keep_indexed(&self, values: &[Value], projection: &[usize]) -> Result<bool, SQLError> {
        evaluate::keep_indexed(&self.expression, values, projection)
    }

    /// Evaluate directly against a composite physical row. Column names and
    /// qualifiers were resolved to logical positions during compilation, so
    /// the hot path neither builds a named row nor allocates a reference list.
    #[inline]
    pub fn keep_row(&self, row: &crate::PhysicalRowView<'_>) -> Result<bool, SQLError> {
        evaluate::keep_row(&self.expression, row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RowSchemaExecution;
    use crate::{eval_scalar, ScalarEvalContext};
    use uqa_sql::ast::ColumnType;
    use uqa_sql::expr::truthy;

    #[test]
    fn predicates_with_outer_attributes_keep_the_scoped_evaluator() {
        let column = uqa_sql::ast::InternalRelationId::allocate().column(0);
        for input in [
            ScalarExpr::InternalColumn(column),
            ScalarExpr::qualified_column("outer", "n"),
            ScalarExpr::ScalarSubquery(0),
        ] {
            let expression = ScalarExpr::Binary {
                op: BinaryOp::Equal,
                lhs: Box::new(input),
                rhs: Box::new(ScalarExpr::Literal(Value::Int(10))),
            };
            assert!(ProjectedPredicate::compile_with_schema(
                &expression,
                &RowSchema::default(),
                &[]
            )
            .unwrap()
            .is_none());
        }
    }

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

        let stored = [Value::Null, Value::Int(3)];
        assert!(predicate.keep_indexed(&stored, &[1, 0]).unwrap());
        assert!(!predicate.keep_indexed(&stored, &[usize::MAX, 0]).unwrap());
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
        let params = vec![SQLParam::typed_scalar(
            Value::Int(365),
            ColumnType::SmallInteger,
        )];
        let predicate = ProjectedPredicate::compile(&q6, &fields, &params)
            .unwrap()
            .unwrap();
        assert!(matches!(
            &predicate.expression,
            ProjectedExpr::IntFieldConjunction(items) if items.len() == 3
        ));
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
    fn projected_integer_comparisons_match_every_canonical_operator_and_operand_order() {
        let fields = vec!["value".into()];
        for op in [
            BinaryOp::Equal,
            BinaryOp::NotEqual,
            BinaryOp::Less,
            BinaryOp::LessEqual,
            BinaryOp::Greater,
            BinaryOp::GreaterEqual,
        ] {
            for field_on_left in [true, false] {
                let field = ScalarExpr::Column("value".into());
                let literal = ScalarExpr::Literal(Value::Int(7));
                let expression = ScalarExpr::Binary {
                    op,
                    lhs: Box::new(if field_on_left {
                        field.clone()
                    } else {
                        literal.clone()
                    }),
                    rhs: Box::new(if field_on_left {
                        literal.clone()
                    } else {
                        field.clone()
                    }),
                };
                let predicate = ProjectedPredicate::compile(&expression, &fields, &[])
                    .unwrap()
                    .unwrap();
                for value in [
                    Value::Null,
                    Value::Int(6),
                    Value::Int(7),
                    Value::Int(8),
                    Value::Float(7.0),
                ] {
                    assert_projected_parity(&expression, &predicate, &fields, &[value], &[]);
                }
            }
        }
    }

    #[test]
    fn projected_like_predicates_match_the_canonical_evaluator() {
        for (operator, pattern) in [
            ("LIKE", "%"),
            ("LIKE", "%green%"),
            ("LIKE", "%special%requests%"),
            ("LIKE", "a_c"),
            ("ILIKE", "%GREEN%"),
        ] {
            let expression = parsed_expression(&format!("text {operator} '{pattern}'"));
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
        let expression = parsed_expression("o.comment NOT LIKE '%special%requests%'");
        let left_schema =
            crate::RowSchema::with_qualified_types("c", vec!["id".into()], vec![None]);
        let right_schema =
            crate::RowSchema::with_qualified_types("o", vec!["comment".into()], vec![None]);
        let schema = crate::RowSchema::join(&left_schema, &right_schema, std::iter::empty());
        let predicate = ProjectedPredicate::compile_with_schema(&expression, &schema, &[])
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

    fn parsed_expression(source: &str) -> ScalarExpr {
        let uqa_sql::Statement::Select(mut query) = uqa_sql::compile(&format!("SELECT {source}"))
            .unwrap()
            .remove(0)
        else {
            panic!("scalar SELECT");
        };
        uqa_sql::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar
    }

    #[test]
    fn session_dependent_datetime_predicates_keep_the_invoking_evaluator() {
        let schema = RowSchema::with_types(
            ["d", "t", "z"].map(str::to_owned).to_vec(),
            [
                ColumnType::Date,
                ColumnType::Timestamp,
                ColumnType::TimestampTz,
            ]
            .map(Some)
            .to_vec(),
        );
        for sql in [
            "d = z",
            "z < d",
            "d BETWEEN z AND z",
            "d IN (z)",
            "d = ANY(ARRAY[TIMESTAMPTZ '2020-01-01+00'])",
            "d <> ALL(ARRAY[TIMESTAMPTZ '2020-01-01+00'])",
        ] {
            assert!(
                ProjectedPredicate::compile_with_schema(&parsed_expression(sql), &schema, &[])
                    .unwrap()
                    .is_none(),
                "{sql}"
            );
        }
        for sql in ["d = d", "d = t", "t = d"] {
            assert!(
                ProjectedPredicate::compile_with_schema(&parsed_expression(sql), &schema, &[])
                    .unwrap()
                    .is_some(),
                "{sql}"
            );
        }
    }

    #[test]
    fn session_dependent_cast_predicates_keep_the_invoking_evaluator() {
        let schema = RowSchema::with_types(
            ["d", "t", "z", "s", "n"].map(str::to_owned).to_vec(),
            [
                ColumnType::Date,
                ColumnType::Timestamp,
                ColumnType::TimestampTz,
                ColumnType::Text,
                ColumnType::Integer,
            ]
            .map(Some)
            .to_vec(),
        );
        for sql in [
            "d::timestamptz = z",
            "t::timestamptz = z",
            "z::timestamp = t",
            "z::date = d",
            "d::timestamp::timestamptz = z",
            "d::text = s",
            "s::date = d",
            "s::timestamptz = z",
        ] {
            assert!(
                ProjectedPredicate::compile_with_schema(&parsed_expression(sql), &schema, &[])
                    .unwrap()
                    .is_none(),
                "{sql}"
            );
        }
        for sql in ["d::timestamp = t", "n::bigint = n", "n::text = s"] {
            assert!(
                ProjectedPredicate::compile_with_schema(&parsed_expression(sql), &schema, &[])
                    .unwrap()
                    .is_some(),
                "{sql}"
            );
        }
        assert!(ProjectedPredicate::compile(
            &parsed_expression("x::smallint = 1"),
            &["x".into()],
            &[],
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn projected_predicate_retains_analyzed_datetime_literals() {
        let expression = ScalarExpr::Binary {
            op: BinaryOp::Less,
            lhs: Box::new(ScalarExpr::Column("day".into())),
            rhs: Box::new(ScalarExpr::Cast {
                implicit: false,
                expr: Box::new(ScalarExpr::Literal(Value::Str("1995-03-15".into()))),
                ty: "date".into(),
            }),
        };
        // Text input still needs the invoking DateStyle. An analyzed temporal constant already owns its value and remains eligible for positional evaluation.
        assert!(
            ProjectedPredicate::compile(&expression, &["day".into()], &[])
                .unwrap()
                .is_none()
        );
        let ScalarExpr::Binary { op, lhs, .. } = expression else {
            unreachable!();
        };
        let expression = ScalarExpr::Binary {
            op,
            lhs,
            rhs: Box::new(ScalarExpr::TypedLiteral {
                value: Value::Temporal(uqa_core::TemporalValue::Date { days: 9204 }),
                ty: "date".into(),
                bound_type: Some(ColumnType::Date),
                collation_oid: None,
                composite_source: None,
                parameter_index: None,
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

    #[test]
    fn projected_predicate_preserves_unary_minus_integer_width() {
        let expression = ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(ScalarExpr::UnaryMinus(Box::new(ScalarExpr::Cast {
                implicit: false,
                expr: Box::new(ScalarExpr::Column("x".into())),
                ty: "smallint".into(),
            }))),
            rhs: Box::new(ScalarExpr::Literal(Value::Int(-1))),
        };
        let schema = RowSchema::with_types(vec!["x".into()], vec![Some(ColumnType::Integer)]);
        let predicate = ProjectedPredicate::compile_with_schema(&expression, &schema, &[])
            .unwrap()
            .unwrap();

        assert!(predicate.keep(&[&Value::Int(1)]).unwrap());
        let error = predicate
            .keep(&[&Value::Int(i64::from(i16::MIN))])
            .expect_err("negating smallint minimum must overflow");
        assert_eq!(error.sqlstate(), Some("22003"));
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

    fn membership(dispatch: &str, operator: &str, elements: Vec<Value>, oid: u32) -> ScalarExpr {
        ScalarExpr::Func {
            name: dispatch.into(),
            binding: Some(uqa_sql::ast::FunctionBinding {
                object_id: None,
                name: if dispatch == "__any_op" {
                    "ANY operator"
                } else {
                    "ALL operator"
                }
                .into(),
                argument_types: Vec::new(),
                builtin: true,
                dispatch: Some(if dispatch == "__any_op" {
                    uqa_sql::ast::FunctionDispatch::AnyOperator
                } else {
                    uqa_sql::ast::FunctionDispatch::AllOperator
                }),
                invocation: None,
                value_binding: None,
                resolution_error: None,
                input_collation: None,
            }),
            args: vec![
                ScalarExpr::Column("mode".into()),
                ScalarExpr::Literal(Value::Array(
                    uqa_core::ArrayValue::try_new(elements)
                        .unwrap()
                        .with_element_type_oid(Some(oid)),
                )),
                ScalarExpr::Literal(Value::Str(operator.into())),
            ],
            distinct: false,
            order_by: Vec::new(),
            order_syntax: uqa_sql::ast::FunctionOrderSyntax::Legacy,
            filter: None,
        }
    }

    #[test]
    fn literal_array_membership_matches_canonical_any_and_all() {
        let modes = || {
            vec![
                Value::FixedChar("AIR".into()),
                Value::FixedChar("AIR REG".into()),
            ]
        };
        let with_null = || vec![Value::Int(1), Value::Null];
        let mut typed = membership("__any_op", "=", modes(), 1042);
        if let ScalarExpr::Func { args, .. } = &mut typed {
            let ScalarExpr::Literal(value) = args[1].clone() else {
                unreachable!("literal array")
            };
            args[1] = ScalarExpr::TypedLiteral {
                value,
                ty: "pg_catalog.\"bpchar\"[]".into(),
                collation_oid: None,
                composite_source: None,
                bound_type: Some(ColumnType::Array(Box::new(ColumnType::Bpchar))),
                parameter_index: None,
            };
        }
        let cases = [
            typed,
            membership("__any_op", "=", modes(), 1042),
            membership("__all_op", "<>", modes(), 1042),
            membership("__any_op", "=", with_null(), 23),
            membership("__all_op", "<>", with_null(), 23),
        ];
        let rows = [
            Value::FixedChar("AIR".into()),
            Value::FixedChar("MAIL".into()),
            Value::Int(1),
            Value::Int(2),
            Value::Null,
        ];
        for expression in &cases {
            let predicate = ProjectedPredicate::compile(expression, &["mode".into()], &[])
                .unwrap()
                .unwrap_or_else(|| panic!("{expression:?} compiles"));
            for value in &rows {
                if matches!(
                    (value, &expression),
                    (Value::Int(_), ScalarExpr::Func { args, .. })
                        if matches!(&args[1], ScalarExpr::Literal(Value::Array(array))
                            if array.element_type_oid() == Some(1042))
                ) {
                    continue;
                }
                let row = uqa_sql::ResultRow::from([("mode".into(), value.clone())]);
                let canonical =
                    eval_scalar(expression, &ScalarEvalContext::new(Some(&row), &[])).unwrap();
                assert_eq!(
                    predicate.keep(&[value]).unwrap(),
                    truthy(&canonical),
                    "{expression:?} with {value:?}"
                );
            }
        }
    }

    #[test]
    fn empty_ordered_or_user_typed_membership_keeps_the_canonical_evaluator() {
        for expression in [
            membership("__any_op", "=", Vec::new(), 23),
            membership("__any_op", "<", vec![Value::Int(1)], 23),
            membership("__all_op", "=", vec![Value::Int(1)], 23),
            membership("__any_op", "=", vec![Value::Int(1)], 16_384),
        ] {
            assert!(
                ProjectedPredicate::compile(&expression, &["mode".into()], &[])
                    .unwrap()
                    .is_none(),
                "{expression:?}"
            );
        }
    }
}
