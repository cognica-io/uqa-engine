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
use crate::{OwnedPhysicalRow, PhysicalRow, RowSchema, RowSchemaExecution, TableScan};
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
