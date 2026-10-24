//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::{
    collection::collect_query_operator, consumer::QueryOutputMode,
    relational::values::test_support::Services, runtime::QueryRuntimeView,
};
use crate::{Batch, OwnedPhysicalRow, PhysicalRow, RowSchema, RowSchemaExecution, TableScan};
use parking_lot::RwLock;
use uqa_core::Value;
use uqa_sql::plan::UnifiedPlan;

#[derive(serde::Deserialize)]
struct Reference {
    sql: String,
    collations: Vec<u32>,
    rows: Vec<Vec<String>>,
}

fn expected_rows(reference: &Reference) -> Vec<Vec<Value>> {
    reference
        .rows
        .iter()
        .map(|row| row.iter().cloned().map(Value::Str).collect())
        .collect()
}

fn materialize(reference: &Reference, spilled: bool) -> QueryOutput {
    let UnifiedPlan::Query(plan) =
        UnifiedPlan::lower(uqa_sql::compile(&reference.sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    let schema = crate::query::binding::analyze_query_plan_schema(
        &Services,
        &plan,
        &[],
        &crate::catalog::test_support::empty_scope(),
        None,
    )
    .unwrap();
    let columns = schema.columns().to_vec();
    let rows = expected_rows(reference)
        .into_iter()
        .map(PhysicalRow::from_values)
        .collect();
    let runtime = QueryRuntimeView {
        function_context: &crate::functions::SQLFunctionContext::empty(),
        diagnostics: &crate::query::diagnostics::QueryDiagnostics::default(),
        cancellation: &uqa_core::CancellationToken::new(),
        settings: &Services,
        scalar_functions: &RwLock::default(),
        table_functions: &RwLock::default(),
        aggregate_functions: &RwLock::default(),
        notices: &crate::query::NoticeQueue::default(),
    };
    collect_query_operator(
        runtime,
        columns,
        Box::new(TableScan::from_physical_rows(schema, rows)),
        if spilled {
            QueryOutputMode::SharedSpill
        } else {
            QueryOutputMode::Rows
        },
    )
    .unwrap()
}

fn check_schema(schema: &RowSchema, reference: &Reference) {
    assert_eq!(schema.len(), reference.collations.len());
    for (position, oid) in reference.collations.iter().enumerate() {
        assert_eq!(schema.column_collation(position), Some(*oid));
    }
}

fn row_values(row: &OwnedPhysicalRow) -> Vec<Value> {
    let view = row.schema.view(&row.row);
    (0..row.schema.len())
        .map(|position| view.value_at(position).unwrap().clone())
        .collect()
}

#[test]
fn materialized_query_outputs_preserve_postgresql_slots_and_collations() {
    let cases: Vec<Reference> =
        serde_json::from_str(include_str!("tests/materialized.json")).unwrap();
    assert_eq!(cases.len(), 2);
    for (case, spilled) in cases
        .iter()
        .flat_map(|case| [false, true].map(|spilled| (case, spilled)))
    {
        let expected = expected_rows(case);
        for public in [false, true] {
            let output = materialize(case, spilled);
            if spilled && !case.rows.is_empty() {
                assert!(matches!(&output.rows, QueryRows::SharedSpill(rows) if rows.has_spilled()));
            }
            let mut operator = if public {
                output.into_public_operator()
            } else {
                output.into_operator()
            };
            let schema = operator.row_schema().clone();
            let actual = crate::physical::run_to_batches(operator.as_mut())
                .unwrap()
                .into_iter()
                .flat_map(|batch| {
                    batch
                        .rows
                        .into_iter()
                        .map(move |row| OwnedPhysicalRow::new(batch.schema.clone(), row))
                })
                .map(|row| row_values(&row))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{}", case.sql);
            check_schema(&schema, case);
        }
        let result = materialize(case, spilled).into_subquery_result().unwrap();
        let actual = result
            .rows
            .map(|row| {
                let row = row.unwrap();
                check_schema(&row.schema, case);
                row_values(&row)
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        let result = materialize(case, spilled).into_sql_result().unwrap();
        check_schema(&result.row_schema(), case);
        assert!(matches!(result.row_schema(), std::borrow::Cow::Borrowed(_)));
        assert_eq!(result.columns, ["x", "x"]);
        assert_eq!(result.positional_rows.as_ref(), Some(&expected));
    }
}

#[test]
fn materialized_spill_batches_preserve_duplicate_positions_and_collations() {
    let mut cases: Vec<Reference> =
        serde_json::from_str(include_str!("tests/materialized.json")).unwrap();
    let reference = cases.remove(0);
    let original = materialize(&reference, false);
    let schema = original.internal_schema;
    let expected: Vec<_> = expected_rows(&reference)
        .into_iter()
        .cycle()
        .take(256)
        .collect();
    let mut buffer = crate::SpillBuffer::new(0);
    for row in &expected {
        buffer
            .push(crate::Batch::from_physical_rows(
                schema.clone(),
                vec![PhysicalRow::from_values(row.clone())],
            ))
            .unwrap();
    }
    let result = QueryOutput {
        columns: original.columns,
        column_types: original.column_types,
        internal_schema: schema.clone(),
        rows: QueryRows::SharedSpill(buffer.into_shared(schema).unwrap()),
    }
    .into_sql_result()
    .unwrap();
    assert_eq!(result.positional_rows.as_ref(), Some(&expected));
    assert_eq!(result.rows.len(), expected.len());
    for (named, positional) in result.rows.iter().zip(&expected) {
        assert_eq!(named.get("x"), Some(&positional[0]));
        assert_eq!(named.get("x_1"), Some(&positional[1]));
    }
    check_schema(&result.row_schema(), &reference);
}

#[test]
fn materialized_batches_reuse_output_capacity_and_projection_metadata() {
    const BATCHES: usize = 64;
    const ROWS: usize = 128;
    let input = RowSchema::new((0..64).map(|column| format!("column_{column}")).collect());
    for schema in [RowSchema::new(Vec::new()), RowSchema::select(&input, &[])] {
        let values = std::sync::Arc::new(vec![Value::Null; schema.physical_width()]);
        let slots: std::sync::Arc<[usize]> = (0..schema.physical_width()).collect();
        let batches = (0..BATCHES)
            .map(|_| {
                Batch::from_physical_rows(
                    schema.clone(),
                    (0..ROWS)
                        .map(|_| {
                            PhysicalRow::from_shared_values(
                                std::sync::Arc::clone(&values),
                                std::sync::Arc::clone(&slots),
                            )
                        })
                        .collect(),
                )
            })
            .collect::<Vec<_>>();
        let mut result = ResultRows::new(&[]);
        result.named.reserve(BATCHES * ROWS);
        let allocations = allocation_counter::measure(|| {
            for batch in batches {
                result.append(&[], batch);
            }
        });
        assert_eq!(result.named.len(), BATCHES * ROWS);
        assert!(result.named.iter().all(uqa_sql::ResultRow::is_empty));
        assert!(result.positional.is_none());
        assert_eq!(std::sync::Arc::strong_count(&values), 1);
        // Output capacity and all input rows are already owned. Allow one 64-slot conversion plan, but no per-batch result buffer or repeated layout plan.
        assert!(
            allocations.bytes_total <= 1024,
            "empty projection allocated {} bytes while appending to reserved output",
            allocations.bytes_total
        );
    }
}
