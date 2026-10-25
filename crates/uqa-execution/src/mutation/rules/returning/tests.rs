//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
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
        _: &dyn uqa_sql::expr::RowLookup,
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

fn capture(
    runtime: QueryRuntimeView<'_>,
    definitions: &[ColumnDef],
    types: Vec<Option<ColumnType>>,
    context: Option<RowSchema>,
    input: impl IntoIterator<Item = Vec<Value>>,
) -> Result<RuleReturningImages, SQLError> {
    use crate::query::consumer::QueryRowConsumer;
    let schema = RowSchema::with_types(vec!["v".into(); types.len()], types);
    let consumer =
        RuleReturningConsumer::new(&Assignment, definitions, None, ReturningRows::new(runtime)?)
            .with_context(context);
    consumer.begin(schema.columns(), &schema)?;
    let mut count = 0;
    for values in input {
        consumer.consume(OwnedPhysicalRow::new(
            schema.clone(),
            PhysicalRow::from_values(values),
        ))?;
        count += 1;
    }
    let RuleReturningResult::Images(images) = consumer.finish(count) else {
        panic!("retained images")
    };
    Ok(images)
}

#[test]
fn native_rule_images_spill_and_preserve_context_without_complete_output() {
    let definitions = columns("text");
    let context_schema = RowSchema::with_qualified_types(
        "source",
        vec!["payload".into()],
        vec![Some(ColumnType::Text)],
    )
    .with_column_collations([Some(950)]);
    with_runtime(64 * 1024, |runtime| {
        let allocated = allocation_counter::measure(|| {
            let captured = capture(
                runtime,
                &definitions,
                vec![Some(ColumnType::Text); 4],
                Some(context_schema.clone()),
                (0..2048).map(|_| {
                    ["c", "o", "n", "s"]
                        .map(|s| Value::Str(s.repeat(8192)))
                        .to_vec()
                }),
            )
            .unwrap();
            assert_eq!(captured.affected_rows, 2048);
            assert_eq!(captured.images.len(), 2048);
            let schema = captured.context_schema.unwrap();
            assert!(schema.shares_index(&context_schema));
            for image in captured.images.into_rows().unwrap() {
                let values = image.unwrap().row.into_physical_values();
                for (index, byte) in [b'c', b'o', b'n', b's'].into_iter().enumerate() {
                    let Value::Str(text) = &values[index] else {
                        panic!("text image")
                    };
                    assert_eq!(text.len(), 8192);
                    assert!(text.bytes().all(|actual| actual == byte));
                }
                let source = source_context(&values, 3, Some(&schema)).unwrap().unwrap();
                assert!(source.schema.shares_index(&context_schema));
                assert_eq!(source.row.value(0), Some(&values[3]));
            }
        });
        // The 64 MiB result retains only bounded encoded pages and the current delivered row.
        assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
    });
}

#[test]
fn native_rule_images_keep_positional_assignment_nulls_and_context_values() {
    let definitions = columns("numeric(5,1)");
    let context = RowSchema::with_qualified_types(
        "source",
        vec!["id".into()],
        vec![Some(ColumnType::Integer)],
    );
    with_runtime(1, |runtime| {
        let captured = capture(
            runtime,
            &definitions,
            vec![Some(ColumnType::Integer); 4],
            Some(context.clone()),
            [
                vec![Value::Int(1), Value::Null, Value::Int(3), Value::Int(91)],
                vec![Value::Int(4), Value::Int(5), Value::Null, Value::Int(37)],
            ],
        )
        .unwrap();
        let rows = captured
            .images
            .into_rows()
            .unwrap()
            .map(|row| row.unwrap().row.into_physical_values())
            .collect::<Vec<_>>();
        for (values, expected) in rows
            .iter()
            .zip([[Some(1), None, Some(3)], [Some(4), Some(5), None]])
        {
            for (value, expected) in values.iter().zip(expected) {
                assert_eq!(
                    *value,
                    expected.map_or(Value::Null, |integer| Value::Decimal(
                        uqa_core::DecimalValue::parse(&format!("{integer}.0")).unwrap()
                    ))
                );
            }
        }
        assert_eq!(
            source_context(&rows[0], 3, Some(&context))
                .unwrap()
                .unwrap()
                .row
                .value(0),
            Some(&Value::Int(91))
        );
        assert_eq!(
            source_context(&rows[1], 3, Some(&context))
                .unwrap()
                .unwrap()
                .row
                .value(0),
            Some(&Value::Int(37))
        );
    });
}

#[test]
fn native_rule_images_reject_incomplete_descriptors_and_observe_cancellation() {
    let definitions = columns("integer");
    let context = RowSchema::new(vec!["source".into()]);
    with_runtime(1, |runtime| {
        let error = capture(
            runtime,
            &definitions,
            vec![Some(ColumnType::Integer); 3],
            Some(context),
            [],
        )
        .err()
        .unwrap();
        assert!(
            matches!(error, SQLError::Internal(message) if message == "rewrite-rule RETURNING provider produced 3 columns, expected 4")
        );
    });
    with_runtime(1, |runtime| {
        runtime.cancellation.cancel();
        let error = capture(
            runtime,
            &definitions,
            vec![Some(ColumnType::Integer); 3],
            None,
            [vec![Value::Int(1), Value::Int(2), Value::Int(3)]],
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("57014"));
    });
}
