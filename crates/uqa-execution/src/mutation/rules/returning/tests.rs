//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mutation::rules::RuleRowImage;
use crate::query::runtime::{QueryMemorySettings, QueryRuntimeView};
use parking_lot::RwLock;
use uqa_core::CancellationToken;
use uqa_sql::{ast::ColumnDef, ColumnType};

struct Assignment;

impl uqa_sql::expr::EngineHook for Assignment {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
}

impl uqa_sql::catalog::domain::DomainCatalog for Assignment {
    fn domain_by_oid(&self, _: u32) -> Option<uqa_sql::catalog::domain::StoredDomain> {
        None
    }
}

impl uqa_sql::assignment::AssignmentContext for Assignment {
    fn evaluate_domain_check(
        &self,
        _: &uqa_sql::ast::Expr,
        _: &uqa_sql::ResultRow,
        _: &RowSchema,
    ) -> Result<Value, SQLError> {
        unreachable!()
    }
}

struct Memory(usize);
impl QueryMemorySettings for Memory {
    fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        Ok(self.0)
    }
}

fn with_runtime(bytes: usize, check: impl FnOnce(QueryRuntimeView<'_>)) {
    check(QueryRuntimeView {
        function_context: &crate::functions::SQLFunctionContext::empty(),
        diagnostics: &crate::query::diagnostics::QueryDiagnostics::default(),
        cancellation: &CancellationToken::new(),
        settings: &Memory(bytes),
        scalar_functions: &RwLock::default(),
        table_functions: &RwLock::default(),
        aggregate_functions: &RwLock::default(),
        notices: &crate::query::NoticeQueue::default(),
    });
}

fn columns(ty: &str) -> Vec<ColumnDef> {
    let uqa_sql::Statement::CreateTable(table) =
        uqa_sql::compile(&format!("CREATE TABLE event_rows (v {ty})"))
            .unwrap()
            .remove(0)
    else {
        panic!("table definition")
    };
    table.columns
}

fn action_output(
    runtime: QueryRuntimeView<'_>,
    types: Vec<Option<ColumnType>>,
    input: impl IntoIterator<Item = Vec<Value>>,
) -> MutationOutput {
    // Repeated labels require positional capture: a named SQL row would discard two images.
    let schema = RowSchema::with_types(vec!["v".into(); types.len()], types);
    let mut rows = ReturningRows::new(runtime).unwrap();
    for values in input {
        rows.push(OwnedPhysicalRow::new(
            schema.clone(),
            PhysicalRow::from_values(values),
        ))
        .unwrap();
    }
    let count = rows.len() as u64;
    MutationOutput::from_returning(schema, rows, count).unwrap()
}

#[test]
fn captured_rule_images_spill_and_share_original_contexts_without_complete_output() {
    let definitions = columns("text");
    let context_schema = RowSchema::with_qualified_types(
        "source",
        vec!["payload".into()],
        vec![Some(ColumnType::Text)],
    )
    .with_column_collations([Some(950)]);
    let mut source = RuleRowImage::empty();
    source.context = Some(OwnedPhysicalRow::new(
        context_schema.clone(),
        PhysicalRow::from_values(vec![Value::Str("source".repeat(350_000))]),
    ));
    let sources = RuleRows::from_rows([Ok(source)], 64 * 1024, CancellationToken::new()).unwrap();
    with_runtime(64 * 1024, |runtime| {
        let allocated = allocation_counter::measure(|| {
            let output = action_output(
                runtime,
                vec![
                    Some(ColumnType::Text),
                    Some(ColumnType::Text),
                    Some(ColumnType::Text),
                    Some(ColumnType::BigInteger),
                ],
                (0..2048).map(|_| {
                    vec![
                        Value::Str("c".repeat(8192)),
                        Value::Str("o".repeat(8192)),
                        Value::Str("n".repeat(8192)),
                        Value::Int(0),
                    ]
                }),
            );
            let captured = capture_rule_returning_result(
                ReturningRows::new(runtime).unwrap(),
                &Assignment,
                output,
                &definitions,
                Some(sources.clone()),
            )
            .unwrap();
            assert_eq!(captured.affected_rows, 2048);
            assert_eq!(captured.images.len(), 2048);
            assert_eq!(captured.source_rows.as_ref().unwrap().len(), 1);
            let mut count = 0;
            for image in captured.images.into_rows().unwrap() {
                let values = image.unwrap().row.into_physical_values();
                for (index, byte) in [b'c', b'o', b'n'].into_iter().enumerate() {
                    let Value::Str(text) = &values[index] else {
                        panic!("text image")
                    };
                    assert_eq!(text.len(), 8192);
                    assert!(text.bytes().all(|actual| actual == byte));
                }
                assert_eq!(
                    source_index(&values, 3, captured.source_rows.as_ref()).unwrap(),
                    Some(0)
                );
                count += 1;
            }
            assert_eq!(count, 2048);
        });
        // The 48 MiB image set and 2.1 MiB source context are never copied into complete output vectors.
        assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
    });
    let source = sources.get(0).unwrap().context.unwrap();
    assert!(source.schema.shares_index(&context_schema));
    assert_eq!(
        source.row.value(0),
        Some(&Value::Str("source".repeat(350_000)))
    );
}

#[test]
fn captured_rule_images_keep_positional_assignment_nulls_and_original_source_indices() {
    let definitions = columns("numeric(5,1)");
    let sources = RuleRows::from_rows(
        [Ok(RuleRowImage::empty()), Ok(RuleRowImage::empty())],
        1,
        CancellationToken::new(),
    )
    .unwrap();
    with_runtime(1, |runtime| {
        let output = action_output(
            runtime,
            vec![Some(ColumnType::Integer); 4],
            [
                vec![Value::Int(1), Value::Null, Value::Int(3), Value::Int(1)],
                vec![Value::Int(4), Value::Int(5), Value::Null, Value::Int(0)],
            ],
        );
        let captured = capture_rule_returning_result(
            ReturningRows::new(runtime).unwrap(),
            &Assignment,
            output,
            &definitions,
            Some(sources.clone()),
        )
        .unwrap();
        let rows = captured
            .images
            .into_rows()
            .unwrap()
            .map(|row| row.unwrap().row.into_physical_values())
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        for (values, expected) in rows
            .iter()
            .zip([[Some(1), None, Some(3)], [Some(4), Some(5), None]])
        {
            for (value, expected) in values.iter().zip(expected) {
                match expected {
                    Some(integer) => assert_eq!(
                        *value,
                        Value::Decimal(
                            uqa_core::DecimalValue::parse(&format!("{integer}.0")).unwrap()
                        )
                    ),
                    None => assert_eq!(*value, Value::Null),
                }
            }
        }
        assert_eq!(source_index(&rows[0], 3, Some(&sources)).unwrap(), Some(1));
        assert_eq!(source_index(&rows[1], 3, Some(&sources)).unwrap(), Some(0));
    });
}

#[test]
fn captured_rule_images_reject_invalid_source_indices_and_observe_cancellation() {
    let definitions = columns("integer");
    let sources =
        RuleRows::from_rows([Ok(RuleRowImage::empty())], 1, CancellationToken::new()).unwrap();
    for index in [
        Value::Int(-1),
        Value::Int(1),
        Value::Null,
        Value::Str("0".into()),
    ] {
        with_runtime(1, |runtime| {
            let output = action_output(
                runtime,
                vec![Some(ColumnType::Integer); 4],
                [vec![Value::Int(1), Value::Int(2), Value::Int(3), index]],
            );
            let error = capture_rule_returning_result(
                ReturningRows::new(runtime).unwrap(),
                &Assignment,
                output,
                &definitions,
                Some(sources.clone()),
            )
            .err()
            .unwrap();
            assert!(
                matches!(error, SQLError::Internal(message) if message.starts_with("rewrite-rule RETURNING source index"))
            );
        });
    }
    with_runtime(1, |runtime| {
        let output = action_output(
            runtime,
            vec![Some(ColumnType::Integer); 3],
            [vec![Value::Int(1), Value::Int(2), Value::Int(3)]],
        );
        let error = capture_rule_returning_result(
            ReturningRows::new(runtime).unwrap(),
            &Assignment,
            output,
            &definitions,
            Some(sources.clone()),
        )
        .err()
        .unwrap();
        assert!(
            matches!(error, SQLError::Internal(message) if message == "rewrite-rule RETURNING provider produced 3 columns, expected 4")
        );
    });
    with_runtime(1, |runtime| {
        let output = action_output(
            runtime,
            vec![Some(ColumnType::Integer); 3],
            [vec![Value::Int(1), Value::Int(2), Value::Int(3)]],
        );
        runtime.cancellation.cancel();
        let error = capture_rule_returning_result(
            ReturningRows::new(runtime).unwrap(),
            &Assignment,
            output,
            &definitions,
            None,
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("57014"));
    });
    assert!(sources.get(0).unwrap().context.is_none());
}
