//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use crate::tests::providers::Provider;
use std::sync::Arc;
use uqa_core::{RelationIdentity, Value};
use uqa_sql::ast::{Expr, PartitionBound, PartitionRangeDatum};

#[derive(Debug)]
struct FailedLiteral;

impl uqa_sql::ir::ConstantValueSource for FailedLiteral {
    fn read(&self, _index: u64) -> Result<Value, uqa_sql::SQLError> {
        Err(uqa_sql::SQLError::Routine {
            sqlstate: "XX001".into(),
            message: "unavailable evaluated definition".into(),
        })
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn failed_native_table_definition_publication_restores_the_original_default(
    #[case] provider: Provider,
) {
    use uqa_sql::ir::{RetainedConstantValue, RetainedScalarLiteral};
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("definitions.db"));
    sql(
        &engine,
        "CREATE TABLE saved(payload text DEFAULT 'original')",
    );
    let source: Arc<dyn uqa_sql::ir::ConstantValueSource> = Arc::new(FailedLiteral);
    let weak = Arc::downgrade(&source);
    let literal = RetainedScalarLiteral::new(
        RetainedConstantValue::new(source, 0),
        Some(uqa_sql::ColumnType::Text),
        None,
        None,
    );
    assert!(engine
        .set_column_default(
            "saved",
            "payload",
            Some(Expr::RetainedLiteral(Box::new(
                uqa_sql::ast::RetainedInputLiteral(literal)
            )))
        )
        .is_err());
    assert!(weak.upgrade().is_none());
    assert_eq!(
        engine
            .sql("INSERT INTO saved DEFAULT VALUES RETURNING payload", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Str("original".into()))
    );
    sql(
        &engine,
        "ALTER TABLE saved ALTER COLUMN payload SET DEFAULT 'replacement'",
    );
    assert_eq!(
        engine
            .sql("INSERT INTO saved DEFAULT VALUES RETURNING payload", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Str("replacement".into()))
    );
    engine.close().unwrap();
}

#[test]
fn committed_memory_table_expressions_capture_original_native_generations() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE parent(payload text DEFAULT 'original', CHECK(payload <> 'forbidden')) PARTITION BY RANGE(payload); CREATE TABLE saved PARTITION OF parent FOR VALUES FROM ('a') TO ('z')");
    let table = engine.table("saved").unwrap().unwrap();
    table
        .columns
        .write()
        .edit(|columns| {
            columns[0].missing_value = Some(Value::Str("m".repeat(1 << 20)));
            Ok(())
        })
        .unwrap();
    table
        .table_checks
        .write()
        .edit(|checks| {
            let Expr::Binary { rhs, .. } = &mut checks[0].expr else {
                panic!("comparison")
            };
            **rhs = Expr::Literal(Value::Str("c".repeat(1 << 20)));
            Ok(())
        })
        .unwrap();
    table
        .hierarchy
        .write()
        .edit(|hierarchy| {
            let Some(PartitionBound::Range { lower, .. }) = &mut hierarchy.partition_bound else {
                panic!("range")
            };
            lower[0] = PartitionRangeDatum::Value(Expr::Literal(Value::Str("b".repeat(1 << 20))));
            Ok(())
        })
        .unwrap();
    let columns = table.columns.generation();
    let checks = table.table_checks.generation();
    let hierarchy = table.hierarchy.generation();
    sql(&engine, "BEGIN");
    table.columns.write().publish(Vec::new()).unwrap();
    table.table_checks.write().publish(Vec::new()).unwrap();
    table
        .hierarchy
        .write()
        .publish(uqa_sql::ast::TableHierarchy::default())
        .unwrap();
    let control = StorageReadControl::with_limit(8 << 20);
    let mut saved = None;
    let allocations = allocation_counter::measure(|| {
        let cut = engine.capture_committed_memory(&control).unwrap();
        saved = Some(
            cut.table(&RelationIdentity::new("public", "saved"))
                .unwrap(),
        );
    });
    assert!(allocations.bytes_max < 64 << 10, "{allocations:?}");
    assert!(control.memory().used() >= 3 << 20);
    let saved = saved.unwrap();
    assert!(saved.columns.get().unwrap().shares_definition(&columns));
    assert!(saved.table_checks.get().unwrap().shares_definition(&checks));
    assert!(saved.hierarchy.get().unwrap().shares_definition(&hierarchy));
    sql(&engine, "ROLLBACK");
    assert!(table.columns.generation().shares_definition(&columns));
    assert!(table.table_checks.generation().shares_definition(&checks));
    assert!(table.hierarchy.generation().shares_definition(&hierarchy));
    table.columns.write().publish(Vec::new()).unwrap();
    table.table_checks.write().publish(Vec::new()).unwrap();
    table
        .hierarchy
        .write()
        .publish(uqa_sql::ast::TableHierarchy::default())
        .unwrap();
    drop((table, engine, columns, checks, hierarchy));
    assert!(
        matches!(&saved.columns.get().unwrap()[0].missing_value, Some(Value::Str(value)) if value.len() == 1 << 20)
    );
    assert!(!saved.table_checks.get().unwrap().is_empty());
    assert!(saved.hierarchy.get().unwrap().partition_bound.is_some());
    control.cancellation().cancel();
    assert!(saved.columns.get().is_err());
    drop(saved);
    assert_eq!(control.memory().used(), 0);
}
