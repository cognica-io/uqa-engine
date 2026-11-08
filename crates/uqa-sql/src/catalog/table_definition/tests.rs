//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnDef, Expr, RetainedInputLiteral},
    ir::{ConstantValueSource, RetainedConstantValue, RetainedScalarLiteral},
};
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::Value;

#[derive(Debug)]
struct Source(Arc<AtomicUsize>);
impl ConstantValueSource for Source {
    fn read(&self, index: u64) -> Result<Value, SQLError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if index != 0 {
            return Err(SQLError::Internal("unavailable source".into()));
        }
        Ok(Value::Str("x".repeat(1 << 20)))
    }
}

fn column(source: Arc<dyn ConstantValueSource>, index: u64) -> ColumnDef {
    let mut column = ColumnDef::nullable("payload", crate::ColumnType::Text);
    column.default = Some(Expr::RetainedLiteral(Box::new(RetainedInputLiteral(
        RetainedScalarLiteral::new(
            RetainedConstantValue::new(source, index),
            Some(crate::ColumnType::Text),
            Some("text".into()),
            Some(100),
        ),
    ))));
    column
}

#[test]
fn published_table_literals_release_query_sources_and_capture_without_reading_or_allocation() {
    let reads = Arc::new(AtomicUsize::new(0));
    let source: Arc<dyn ConstantValueSource> = Arc::new(Source(reads.clone()));
    let weak = Arc::downgrade(&source);
    let columns = vec![column(source, 0)];
    let native = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let definition =
        TableDefinition::with_control(&columns, &ProductionControl::new(&native, &token, &token))
            .unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(native.used(), definition.retained_bytes());
    assert!(native.used() > 1 << 20);
    drop(columns);
    assert!(weak.upgrade().is_none());
    let bytes = definition.retained_bytes();
    let memory = MemoryBudget::new(bytes);
    let control = ProductionControl::new(&memory, &token, &token);
    let mut captured = None;
    let allocations = allocation_counter::measure(|| {
        captured = Some(definition.capture_with_control(&control).unwrap());
    });
    assert_eq!(allocations.count_total, 0, "{allocations:?}");
    let captured = captured.unwrap();
    assert!(definition.shares_definition(&captured));
    assert_eq!(memory.used(), bytes);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    let denied = MemoryBudget::new(bytes - 1);
    assert!(definition
        .capture_with_control(&ProductionControl::new(&denied, &token, &token))
        .is_err());
    assert_eq!(denied.used(), 0);
    let shared = definition.shared();
    drop(definition);
    drop(captured);
    assert_eq!(memory.used(), 0);
    assert_eq!(native.used(), bytes);
    let Some(Expr::RetainedLiteral(literal)) = &shared[0].default else {
        panic!("literal")
    };
    assert!(
        matches!(literal.0.value.resident_value(), Some(Value::Str(value)) if value.len() == 1 << 20)
    );
    assert_eq!(literal.0.collation_oid, Some(100));
    drop(shared);
    assert_eq!(native.used(), 0);
}

#[test]
fn table_definition_publication_keeps_the_original_on_source_failure_and_unwinding() {
    let mut definition = TableDefinition::new(&vec![ColumnDef::nullable(
        "original",
        crate::ColumnType::Integer,
    )])
    .unwrap();
    let original = definition.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    {
        let mut write = TableDefinition::write(&mut definition);
        assert!(write
            .publish(vec![column(Arc::new(Source(reads.clone())), 1)])
            .is_err());
    }
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert!(definition.shares_definition(&original));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut write = TableDefinition::write(&mut definition);
        let _: Result<(), SQLError> = write.edit(|columns| {
            columns.clear();
            panic!("candidate unwind")
        });
    }));
    assert!(result.is_err());
    assert!(definition.shares_definition(&original));
    {
        let mut write = TableDefinition::write(&mut definition);
        write
            .edit(|columns| {
                columns[0].name = "replacement".into();
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(original[0].name, "original");
    assert_eq!(definition[0].name, "replacement");
    assert!(!definition.shares_definition(&original));
}

#[test]
fn published_table_definitions_preserve_constraints_and_provisional_syntax() {
    for sql in [
        "CREATE TABLE t(id integer GENERATED ALWAYS AS IDENTITY, v text DEFAULT 'original' CHECK(length(v)>0), CONSTRAINT positive CHECK(id>0))",
        "CREATE TABLE leaf PARTITION OF parent FOR VALUES FROM (1) TO (9)",
        "CREATE TABLE pending(id integer DEFAULT (SELECT 1))",
    ] {
        let crate::Statement::CreateTable(table) = crate::compile(sql).unwrap().remove(0) else { panic!("table") };
        macro_rules! same { ($value:expr) => {{
            let value = $value;
            let definition = TableDefinition::new(value).unwrap();
            assert_eq!(serde_json::to_value(&definition).unwrap(), serde_json::to_value(value).unwrap());
            let memory = MemoryBudget::new(definition.retained_bytes());
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            invoking.cancel();
            assert!(definition.capture_with_control(&ProductionControl::new(&memory, &original, &invoking)).is_err());
            let invoking = CancellationToken::new();
            original.cancel();
            assert!(definition.capture_with_control(&ProductionControl::new(&memory, &original, &invoking)).is_err());
            assert_eq!(memory.used(), 0);
        }}; }
        same!(&table.columns);
        same!(&table.checks);
        same!(&table.hierarchy);
    }
}

#[test]
fn retained_native_columns_keep_constructor_leases_and_charge_literal_payloads() {
    let reads = Arc::new(AtomicUsize::new(0));
    let columns = vec![column(Arc::new(Source(reads.clone())), 0)];
    let native = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let definition =
        TableDefinition::with_control(&columns, &ProductionControl::new(&native, &token, &token))
            .unwrap();
    let bytes = definition.retained_bytes();
    let memory =
        MemoryBudget::new(bytes + size_of::<Budgeted<SharedTableDefinition<Vec<ColumnDef>>>>());
    let shared = definition.shared();
    let retained =
        crate::schema::retention::RetainedColumns::capture(&shared, &memory, &token).unwrap();
    assert_eq!(memory.used(), memory.limit());
    assert_eq!(retained.as_ptr(), shared.as_ptr());
    drop(shared);
    drop(definition);
    assert_eq!(native.used(), bytes);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    drop(retained);
    assert_eq!(native.used(), 0);
    assert_eq!(memory.used(), 0);
}
