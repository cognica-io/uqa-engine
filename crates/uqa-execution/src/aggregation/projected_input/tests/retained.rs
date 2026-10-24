//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ColumnType;

fn retained(expression: ScalarExpr, ty: ColumnType) -> ScalarExpr {
    ScalarExpr::Collated {
        expr: Box::new(expression),
        ty,
        collation: serde_json::from_value(serde_json::json!({
            "derivation": "None", "initial_oid": 0,
        }))
        .unwrap(),
    }
}

fn product() -> ScalarExpr {
    ScalarExpr::Binary {
        op: BinaryOp::Multiply,
        lhs: Box::new(ScalarExpr::Column("a".into())),
        rhs: Box::new(ScalarExpr::Column("b".into())),
    }
}

struct PositionalRow(Vec<Value>);

impl RowLookup for PositionalRow {
    fn column(&self, _: &str) -> Option<&Value> {
        panic!("retained arithmetic must not repeat named scalar evaluation")
    }

    fn qualified_column(&self, _: &str, _: &str) -> Option<&Value> {
        panic!("retained arithmetic must not repeat qualified scalar evaluation")
    }

    fn positional_column(&self, index: usize) -> Option<&Value> {
        self.0.get(index)
    }
}

#[test]
fn retained_integer_aggregate_metadata_keeps_positional_updates() {
    let schema = RowSchema::with_types(
        vec!["a".into(), "b".into()],
        vec![Some(ColumnType::BigInteger); 2],
    );
    let product = retained(product(), ColumnType::BigInteger);
    let nested = retained(
        ScalarExpr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(product.clone()),
            rhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
        },
        ColumnType::BigInteger,
    );
    let targets = [aggregate("sum", product), aggregate("sum", nested)];
    let plans = ProjectedAggregatePlans::compile(&|_: &str| false, &targets, &schema);
    assert!(
        plans.all_direct(),
        "analyzed Q6 arithmetic lost positional aggregation"
    );
    let mut accumulators = vec![
        AggregateAccumulator::builtin("sum"),
        AggregateAccumulator::builtin("sum"),
    ];
    for values in [
        vec![Value::Int(10), Value::Int(2)],
        vec![Value::Int(7), Value::Int(3)],
        vec![Value::Null, Value::Int(4)],
        vec![Value::Int(-5), Value::Int(2)],
    ] {
        plans
            .observe_direct(&mut accumulators, &PositionalRow(values), &[], None)
            .unwrap();
    }
    // PostgreSQL reference: SUM(a*b)=31, SUM(a*b+1)=34; NULL does not contribute.
    assert_eq!(accumulators[0].integer_sum, 31);
    assert_eq!(accumulators[1].integer_sum, 34);
    assert_eq!(accumulators[0].count, 3);
    assert_eq!(accumulators[1].count, 3);
}

#[test]
fn retained_integer_aggregate_metadata_preserves_operand_width_and_errors() {
    for (ty, maximum) in [
        (ColumnType::SmallInteger, i64::from(i16::MAX)),
        (ColumnType::Integer, i64::from(i32::MAX)),
        (ColumnType::BigInteger, i64::MAX),
    ] {
        let schema = RowSchema::with_types(vec!["a".into(), "b".into()], vec![Some(ty.clone()); 2]);
        let argument = retained(
            ScalarExpr::Binary {
                op: BinaryOp::Add,
                lhs: Box::new(retained(product(), ty.clone())),
                rhs: Box::new(ScalarExpr::Literal(Value::Int(0))),
            },
            if ty == ColumnType::SmallInteger {
                ColumnType::Integer
            } else {
                ty
            },
        );
        let plans = ProjectedAggregatePlans::compile(
            &|_: &str| false,
            &[aggregate("sum", argument)],
            &schema,
        );
        assert!(plans.all_direct());
        let mut accumulators = vec![AggregateAccumulator::builtin("sum")];
        let error = plans
            .observe_direct(
                &mut accumulators,
                &PositionalRow(vec![Value::Int(maximum), Value::Int(2)]),
                &[],
                None,
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"));
        assert_eq!(accumulators[0].count, 0);
    }
}

#[test]
fn retained_integer_aggregate_metadata_preserves_scalar_fallback_and_casts() {
    let schema = RowSchema::with_types(
        vec!["a".into(), "b".into()],
        vec![Some(ColumnType::DoublePrecision), Some(ColumnType::Integer)],
    );
    let targets = [aggregate(
        "sum",
        retained(product(), ColumnType::DoublePrecision),
    )];
    let plans = ProjectedAggregatePlans::compile(&|_: &str| false, &targets, &schema);
    assert!(plans.all_direct());
    let mut accumulators = vec![AggregateAccumulator::builtin("sum")];
    let row = TestRow {
        schema: schema.columns().to_vec(),
        values: vec![Value::Float(1.5), Value::Int(2)],
    };
    plans
        .observe_direct(&mut accumulators, &row, &[], None)
        .unwrap();
    assert_eq!(accumulators[0].sum, 3.0);
    assert_eq!(accumulators[0].count, 1);

    // A narrowing cast is an operation, unlike retained result metadata.
    let targets = [aggregate(
        "sum",
        retained(
            ScalarExpr::Cast {
                expr: Box::new(product()),
                ty: "smallint".into(),
                implicit: false,
            },
            ColumnType::SmallInteger,
        ),
    )];
    let plans = ProjectedAggregatePlans::compile(&|_: &str| false, &targets, &schema);
    assert!(!plans.all_direct());
}
