//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{AggregateExecutor, Batch, ProjectedRow, ProjectedValueSlot};
use uqa_sql::plan::RelationalPlan;

#[test]
fn aggregate_cast_boundaries_keep_postgresql_integer_overflow_in_borrowed_and_owned_rows() {
    let schema = RowSchema::with_types(
        vec!["price".into(), "quantity".into()],
        vec![Some(ColumnType::Integer); 2],
    );
    let fields = [&Value::Int(2_147_483_647), &Value::Int(2)];
    let slots = [ProjectedValueSlot::Field(0), ProjectedValueSlot::Field(1)];
    for sql in [
        "SELECT SUM((price*quantity)::bigint) AS total FROM t",
        "SELECT SUM(DISTINCT (price*quantity)::bigint) AS total FROM t",
    ] {
        let uqa_sql::Statement::Select(select) = uqa_sql::compile(sql).unwrap().remove(0) else {
            panic!("expected SELECT")
        };
        let RelationalPlan::QueryBlock(statement) = QueryPlan::lower(*select).root else {
            panic!("expected query block")
        };
        for borrowed in [false, true] {
            let mut executor = crate::aggregation::PhysicalAggregateExecutor::new(
                Arc::new(Context::default()),
                &statement,
                &[],
                schema.clone(),
                RowSchema::new(vec!["total".into()]),
                1 << 20,
            )
            .unwrap();
            if borrowed && !executor.supports_projected_rows() {
                continue;
            }
            let result = if borrowed {
                executor.consume_projected_row(&ProjectedRow::new(&schema, &slots, &fields, &[]))
            } else {
                executor.consume(Batch::from_physical_rows(
                    schema.clone(),
                    vec![PhysicalRow::from_values(vec![
                        Value::Int(2_147_483_647),
                        Value::Int(2),
                    ])],
                ))
            }
            .and_then(|()| executor.finish().map(|_| ()));
            // PostgreSQL 18.6 raises 22003 before the outer BIGINT cast and before SUM observes the overflowing product.
            let error = result.expect_err("int4 product must overflow before casting");
            assert!(
                error.to_string().contains("integer out of range"),
                "{sql}: {error}"
            );
        }
    }
}
