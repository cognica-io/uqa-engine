//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::consumer::QueryRowConsumer;
use crate::query::runtime::QueryRuntimeView;
use crate::{OwnedPhysicalRow, RowSchema, RowSchemaExecution};
use parking_lot::RwLock;
use std::{cell::RefCell, rc::Rc};
use uqa_core::Value;
use uqa_sql::plan::{RelationalPlan, UnifiedPlan};

use super::test_support::Services;

#[derive(serde::Deserialize)]
struct Reference {
    sql: String,
    collations: Option<Vec<u32>>,
    error: Option<Diagnostic>,
}
#[derive(serde::Deserialize)]
struct Diagnostic {
    sqlstate: String,
    message: String,
    detail: Option<String>,
    hint: Option<String>,
}

#[derive(Default)]
struct Capture {
    schema: RefCell<Option<RowSchema>>,
    values: RefCell<Vec<Vec<Value>>>,
    directional: bool,
}
impl QueryRowConsumer for Capture {
    fn begin(&self, columns: &[String], schema: &RowSchema) -> Result<(), SQLError> {
        assert_eq!(columns, schema.columns());
        self.schema.replace(Some(schema.clone()));
        Ok(())
    }
    fn consume(&self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        let view = row.schema.view(&row.row);
        self.values.borrow_mut().push(
            (0..row.schema.len())
                .map(|position| view.value_at(position).unwrap().clone())
                .collect(),
        );
        Ok(QueryConsumerControl::Continue)
    }
    fn uses_directional_scan(&self) -> bool {
        self.directional
    }
}

fn execute(sql: &str, mode: QueryOutputMode<'_>) -> Result<QueryOutput, SQLError> {
    let UnifiedPlan::Query(plan) = UnifiedPlan::lower(uqa_sql::compile(sql)?.remove(0)) else {
        panic!("query")
    };
    let RelationalPlan::Values { rows, subqueries } = &plan.root else {
        panic!("VALUES")
    };
    let scope = crate::catalog::test_support::empty_scope();
    crate::query::binding::analyze_query_plan_schema(&Services, &plan, &[], &scope, None)?;
    let runtime = QueryRuntimeView {
        diagnostics: &crate::query::diagnostics::QueryDiagnostics::default(),
        cancellation: &uqa_core::CancellationToken::new(),
        settings: &Services,
        scalar_functions: &RwLock::default(),
        table_functions: &RwLock::default(),
        aggregate_functions: &RwLock::default(),
        notices: &crate::query::NoticeQueue::default(),
    };
    let context = RelationalContext {
        catalog: &Services,
        expressions: &Services,
        row_locks: &Services,
        runtime,
    };
    execute_plan_values_output(context, rows, subqueries, &[], &scope, mode)
}

#[test]
fn values_delivery_keeps_postgresql_collations_and_diagnostics() {
    let cases: Vec<Reference> =
        serde_json::from_str(include_str!("tests/collations.json")).unwrap();
    assert_eq!(cases.len(), 12);
    for case in cases {
        for delivery in 0..3 {
            let capture = Rc::new(Capture {
                directional: delivery == 1,
                ..Capture::default()
            });
            let mode = if delivery == 2 {
                QueryOutputMode::SharedSpill
            } else {
                QueryOutputMode::RowConsumer(capture.clone())
            };
            let result = execute(&case.sql, mode);
            if let Some(expected) = &case.collations {
                let output = result.unwrap_or_else(|error| panic!("{}: {error}", case.sql));
                if delivery == 2 {
                    capture_spill(output, &capture);
                }
                let schema = capture.schema.borrow();
                let schema = schema.as_ref().unwrap();
                assert_eq!(schema.len(), expected.len());
                for (position, expected) in expected.iter().enumerate() {
                    assert_eq!(
                        schema.column_collation(position),
                        Some(*expected),
                        "{}",
                        case.sql
                    );
                }
                super::test_support::assert_values(&case.sql, &capture.values.borrow());
            } else {
                let error = result.err().unwrap();
                let expected = case.error.as_ref().unwrap();
                assert_eq!(
                    error.sqlstate(),
                    Some(expected.sqlstate.as_str()),
                    "{}",
                    case.sql
                );
                assert_eq!(error.to_string(), expected.message, "{}", case.sql);
                assert_eq!(error.detail(), expected.detail.as_deref());
                assert_eq!(error.hint(), expected.hint.as_deref());
                assert!(capture.schema.borrow().is_none());
                assert!(capture.values.borrow().is_empty());
            }
        }
    }
}

fn capture_spill(output: QueryOutput, capture: &Capture) {
    assert!(matches!(&output.rows, QueryRows::SharedSpill(rows) if rows.has_spilled()));
    let mut operator = output.into_operator();
    let schema = operator.row_schema();
    capture.begin(schema.columns(), schema).unwrap();
    for batch in crate::physical::run_to_batches(operator.as_mut()).unwrap() {
        for row in batch.rows {
            capture
                .consume(OwnedPhysicalRow::new(batch.schema.clone(), row))
                .unwrap();
        }
    }
}

#[test]
fn materialized_values_retain_metadata_at_query_boundaries() {
    let cases: Vec<Reference> =
        serde_json::from_str(include_str!("tests/collations.json")).unwrap();
    for case in cases {
        let Some(expected) = case.collations else {
            continue;
        };
        let mut operator = execute(&case.sql, QueryOutputMode::Rows)
            .unwrap()
            .into_operator();
        for (position, oid) in expected.iter().enumerate() {
            assert_eq!(
                operator.row_schema().column_collation(position),
                Some(*oid),
                "{}",
                case.sql
            );
        }
        let (_, rows) = crate::physical::run_to_rows(operator.as_mut()).unwrap();
        let values = rows
            .into_iter()
            .map(|row| vec![row["column1"].clone()])
            .collect::<Vec<_>>();
        super::test_support::assert_values(&case.sql, &values);
        let result = execute(&case.sql, QueryOutputMode::Rows)
            .unwrap()
            .into_subquery_result()
            .unwrap();
        let mut values = Vec::new();
        for row in result.rows {
            let row = row.unwrap();
            for (position, oid) in expected.iter().enumerate() {
                assert_eq!(row.schema.column_collation(position), Some(*oid));
            }
            let view = row.schema.view(&row.row);
            values.push(vec![view.value_at(0).unwrap().clone()]);
        }
        super::test_support::assert_values(&case.sql, &values);
    }
}
