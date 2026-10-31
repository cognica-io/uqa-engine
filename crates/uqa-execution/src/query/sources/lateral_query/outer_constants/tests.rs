//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::PhysicalRow;
use uqa_core::Value;
use uqa_sql::ast::BinaryOp;

fn lineitem() -> RowSchema {
    RowSchema::with_qualified_types(
        "lineitem",
        vec!["l_partkey".into(), "l_comment".into()],
        vec![Some(ColumnType::BigInteger), Some(ColumnType::Text)],
    )
}

fn partsupp_row() -> OwnedPhysicalRow {
    OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(
            "partsupp",
            vec!["ps_partkey".into(), "ps_comment".into(), "l_comment".into()],
            vec![
                Some(ColumnType::BigInteger),
                Some(ColumnType::Text),
                Some(ColumnType::BigInteger),
            ],
        ),
        PhysicalRow::from_values(vec![
            Value::Int(5),
            Value::Str("outer".into()),
            Value::Int(9),
        ]),
    )
}

fn equal(lhs: ScalarExpr, rhs: ScalarExpr) -> ScalarExpr {
    ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

fn qualified(qualifier: &str, column: &str) -> ScalarExpr {
    ScalarExpr::QualifiedColumn {
        qualifier: qualifier.into(),
        column: column.into(),
    }
}

#[test]
fn runtime_local_names_are_not_replaced_by_outer_constants() {
    let schema = RowSchema::with_open_columns(&lineitem(), Some("lineitem"));
    let filter = equal(
        ScalarExpr::Column("ps_partkey".into()),
        ScalarExpr::Literal(Value::Int(5)),
    );
    assert_eq!(
        bind_outer_constants(&filter, &schema, &partsupp_row()),
        None
    );
    let qualified = equal(
        qualified("partsupp", "ps_partkey"),
        ScalarExpr::Literal(Value::Int(5)),
    );
    assert!(bind_outer_constants(&qualified, &schema, &partsupp_row()).is_some());
}

#[test]
fn unshadowed_outer_keys_become_typed_literals_of_their_current_values() {
    let filter = ScalarExpr::And(vec![
        equal(
            qualified("lineitem", "l_partkey"),
            qualified("partsupp", "ps_partkey"),
        ),
        equal(
            ScalarExpr::Column("l_partkey".into()),
            ScalarExpr::Column("ps_partkey".into()),
        ),
    ]);
    let bound = bind_outer_constants(&filter, &lineitem(), &partsupp_row()).unwrap();
    let literal = ScalarExpr::TypedLiteral {
        collation_oid: None,
        composite_source: None,
        value: Value::Int(5),
        ty: ColumnType::BigInteger.catalog_name(),
        bound_type: Some(ColumnType::BigInteger),
        parameter_index: None,
    };
    assert_eq!(
        bound,
        ScalarExpr::And(vec![
            equal(qualified("lineitem", "l_partkey"), literal.clone()),
            equal(ScalarExpr::Column("l_partkey".into()), literal),
        ])
    );
}

#[test]
fn shadowed_collatable_and_inner_references_stay_columns() {
    for filter in [
        // The block's own column shadows the outer column of the same name.
        equal(
            ScalarExpr::Column("l_comment".into()),
            ScalarExpr::Literal(Value::Int(1)),
        ),
        // A collatable outer value keeps its column collation.
        equal(
            qualified("lineitem", "l_comment"),
            qualified("partsupp", "ps_comment"),
        ),
        // A qualifier of the block's own sources never reads the outer row.
        equal(
            qualified("lineitem", "l_partkey"),
            qualified("lineitem", "l_partkey"),
        ),
    ] {
        assert_eq!(
            bind_outer_constants(&filter, &lineitem(), &partsupp_row()),
            None,
            "{filter:?}"
        );
    }
}

#[test]
fn outer_constant_rewrites_admit_payloads_and_keep_independent_readers() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let schema = lineitem();
    let row = partsupp_row();
    let filter = ScalarExpr::And(vec![
        equal(
            qualified("partsupp", "ps_partkey"),
            ScalarExpr::Literal(Value::Int(5)),
        ),
        ScalarExpr::Literal(Value::Str("x".repeat(4 << 20))),
    ]);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let tiny = MemoryBudget::new(32 << 10);
    let control = ProductionControl::new(&tiny, &original, &invoking);
    for failure in 0..3 {
        if failure == 1 {
            original.cancel();
        }
        if failure == 2 {
            invoking.cancel();
        }
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                bind_outer_constants_with_control(&filter, &schema, &row, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some(if failure == 0 { "53200" } else { "57014" })
            );
        });
        assert!(allocation.bytes_total < 128 << 10, "{allocation:?}");
        assert_eq!(tiny.used(), 0);
        original.reset();
        invoking.reset();
    }
    let budget = MemoryBudget::new(32 << 20);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = bind_outer_constants_with_control(&filter, &schema, &row, &control).unwrap();
    });
    assert_eq!(allocation.bytes_current, budget.used() as i64);
    let output = output.unwrap();
    let ScalarExpr::And(parts) = &*output else {
        panic!("conjunction")
    };
    assert_eq!(
        parts[0],
        equal(
            ScalarExpr::TypedLiteral {
                value: Value::Int(5),
                ty: r#"pg_catalog."int8""#.into(),
                bound_type: Some(ColumnType::BigInteger),
                collation_oid: None,
                composite_source: None,
                parameter_index: None
            },
            ScalarExpr::Literal(Value::Int(5))
        )
    );
    drop(output);
    assert_eq!(budget.used(), 0);
}
