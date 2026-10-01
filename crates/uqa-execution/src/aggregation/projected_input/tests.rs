//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;
use uqa_sql::ast::BinaryOp;
use uqa_sql::expr::RowLookup;

struct TestRow {
    schema: Vec<String>,
    values: Vec<Value>,
}

impl RowLookup for TestRow {
    fn column(&self, name: &str) -> Option<&Value> {
        self.schema
            .iter()
            .position(|column| column == name)
            .and_then(|index| self.values.get(index))
    }

    fn qualified_column(&self, _qualifier: &str, column: &str) -> Option<&Value> {
        self.column(column)
    }

    fn positional_column(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }
}

fn aggregate(name: &str, argument: ScalarExpr) -> ScalarExpr {
    ScalarExpr::Func {
        name: name.into(),
        binding: None,
        args: vec![argument],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

#[test]
fn integer_expression_and_count_use_positional_state_updates() {
    let context = |_: &str| false;
    let expression = ScalarExpr::Binary {
        op: BinaryOp::Multiply,
        lhs: Box::new(ScalarExpr::Column("a".into())),
        rhs: Box::new(ScalarExpr::Column("b".into())),
    };
    let targets = vec![
        aggregate("sum", expression),
        aggregate("count", ScalarExpr::Star),
        aggregate("avg", ScalarExpr::Column("a".into())),
    ];
    let schema = RowSchema::new(vec!["a".into(), "b".into()]);
    let plans = ProjectedAggregatePlans::compile(&context, &targets, &schema);
    let mut accumulators = vec![
        AggregateAccumulator::builtin("sum"),
        AggregateAccumulator::builtin("count"),
        AggregateAccumulator::builtin("avg"),
    ];
    let row = TestRow {
        schema: vec!["a".into(), "b".into()],
        values: vec![Value::Int(3), Value::Int(4)],
    };
    assert!(plans.all_direct());
    plans.observe_direct(&mut accumulators, &row, &[]).unwrap();

    assert_eq!(accumulators[0].integer_sum, 12);
    assert_eq!(accumulators[0].count, 1);
    assert_eq!(accumulators[1].count, 1);
    assert_eq!(accumulators[2].integer_sum, 3);
    assert_eq!(accumulators[2].count, 1);
}

#[test]
fn non_integer_input_falls_back_to_canonical_expression_evaluation() {
    let context = |_: &str| false;
    let expression = ScalarExpr::Binary {
        op: BinaryOp::Multiply,
        lhs: Box::new(ScalarExpr::Column("a".into())),
        rhs: Box::new(ScalarExpr::Column("b".into())),
    };
    let targets = vec![aggregate("sum", expression)];
    let schema = RowSchema::new(vec!["a".into(), "b".into()]);
    let plans = ProjectedAggregatePlans::compile(&context, &targets, &schema);
    let mut accumulators = vec![AggregateAccumulator::builtin("sum")];
    let row = TestRow {
        schema: vec!["a".into(), "b".into()],
        values: vec![Value::Float(1.5), Value::Int(2)],
    };
    assert!(plans.all_direct());
    plans.observe_direct(&mut accumulators, &row, &[]).unwrap();

    assert_eq!(accumulators[0].count, 1);
    assert_eq!(accumulators[0].sum, 3.0);
}

#[test]
fn bigint_operand_cast_preserves_postgresql_widening_and_null_aggregation() {
    let expression = ScalarExpr::Binary {
        op: BinaryOp::Multiply,
        lhs: Box::new(ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Column("price".into())),
            ty: "bigint".into(),
        }),
        rhs: Box::new(ScalarExpr::Column("quantity".into())),
    };
    let targets = vec![aggregate("sum", expression)];
    let schema = RowSchema::new(vec!["price".into(), "quantity".into()]);
    let plans = ProjectedAggregatePlans::compile(&|_: &str| false, &targets, &schema);
    assert!(plans.all_direct());
    let mut accumulators = vec![AggregateAccumulator::builtin("sum")];
    for values in [
        vec![Value::Int(2_147_483_647), Value::Int(2)],
        vec![Value::Null, Value::Int(3)],
        vec![Value::Int(-2_147_483_648), Value::Int(2)],
    ] {
        plans
            .observe_direct(
                &mut accumulators,
                &TestRow {
                    schema: schema.columns().to_vec(),
                    values,
                },
                &[],
            )
            .unwrap();
    }
    // Independent PostgreSQL 18.6: SUM(price::bigint * quantity) over these three rows is -2; the NULL product does not contribute.
    assert_eq!(accumulators[0].integer_sum, -2);
    assert_eq!(accumulators[0].count, 2);
}

#[test]
fn projected_integer_arithmetic_preserves_postgresql_operand_widths() {
    use uqa_sql::ast::ColumnType;
    for (ty, maximum) in [
        (ColumnType::SmallInteger, i64::from(i16::MAX)),
        (ColumnType::Integer, i64::from(i32::MAX)),
    ] {
        let schema = RowSchema::with_types(
            vec!["price".into(), "quantity".into()],
            vec![Some(ty.clone()), Some(ty)],
        );
        let row = TestRow {
            schema: schema.columns().to_vec(),
            values: vec![Value::Int(maximum), Value::Int(2)],
        };
        for argument in [
            ScalarExpr::Binary {
                op: BinaryOp::Multiply,
                lhs: Box::new(ScalarExpr::Column("price".into())),
                rhs: Box::new(ScalarExpr::Column("quantity".into())),
            },
            ScalarExpr::Binary {
                op: BinaryOp::Add,
                lhs: Box::new(ScalarExpr::Column("quantity".into())),
                rhs: Box::new(ScalarExpr::Binary {
                    op: BinaryOp::Multiply,
                    lhs: Box::new(ScalarExpr::Column("price".into())),
                    rhs: Box::new(ScalarExpr::Column("quantity".into())),
                }),
            },
        ] {
            let plans = ProjectedAggregatePlans::compile(
                &|_: &str| false,
                &[aggregate("sum", argument)],
                &schema,
            );
            assert!(plans.all_direct());
            let mut accumulators = vec![AggregateAccumulator::builtin("sum")];
            // Independent PostgreSQL 18.6 reports 22003 for both int2 and int4 multiplication before SUM observes a row.
            let error = plans
                .observe_direct(&mut accumulators, &row, &[])
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("22003"));
            assert_eq!(accumulators[0].count, 0);
        }
    }
}

#[test]
fn positional_compilation_resolves_duplicate_names_by_structured_qualifier() {
    use crate::ColumnIdentity;

    let schema = RowSchema::with_identities(
        vec!["name".into(), "name".into()],
        vec![
            ColumnIdentity::qualified("employee", "name"),
            ColumnIdentity::qualified("department", "name"),
        ],
        vec![None, None],
    );
    assert_eq!(
        column_slot(&ScalarExpr::qualified_column("department", "name"), &schema),
        Some(1)
    );
    assert_eq!(
        column_slot(&ScalarExpr::Column("name".into()), &schema),
        None
    );
}
