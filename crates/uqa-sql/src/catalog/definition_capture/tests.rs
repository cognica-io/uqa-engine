//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn table(sql: &str) -> crate::ast::CreateTable {
    let crate::Statement::CreateTable(table) = crate::compile(sql).unwrap().remove(0) else {
        panic!("table definition")
    };
    table
}

#[test]
fn captured_columns_detach_retained_literal_payloads_into_the_capture_allowance() {
    let original = MemoryBudget::new(2 << 20);
    let cancellation = CancellationToken::new();
    let input = ProductionControl::new(&original, &cancellation, &cancellation);
    let value = input.copy_value(&Value::Str("x".repeat(1 << 20))).unwrap();
    let value = crate::ir::RetainedConstantValue::from_produced(value, &input).unwrap();
    let literal = crate::ir::RetainedScalarLiteral::with_control(
        value,
        Some(crate::ColumnType::Text.clone_with_control(&input).unwrap()),
        Some(input.copy_text("text").unwrap()),
        Some(100),
        &input,
    )
    .unwrap();
    let mut column = crate::ast::ColumnDef::nullable("payload", crate::ColumnType::Text);
    column.default = Some(crate::ast::Expr::RetainedLiteral(Box::new(
        crate::ast::RetainedInputLiteral(literal),
    )));
    let statement = CancellationToken::new();
    statement.cancel();
    let _statement = crate::expr::StatementCancellationScope::enter(statement);
    let Some(crate::ast::Expr::RetainedLiteral(source)) = &column.default else {
        unreachable!()
    };
    assert!(matches!(
        source.0.value.read_with_control(&input),
        Err(SQLError::Cancelled(_))
    ));
    let small = MemoryBudget::new(4096);
    let control = ProductionControl::new(&small, &cancellation, &cancellation);
    assert_eq!(
        columns(std::slice::from_ref(&column), &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(small.used(), 0);
    let memory = MemoryBudget::new(2 << 20);
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let copied = columns(std::slice::from_ref(&column), &control)
        .unwrap()
        .into_budgeted()
        .unwrap();
    let Some(crate::ast::Expr::RetainedLiteral(captured)) = &copied[0].default else {
        panic!("retained literal")
    };
    let Some(crate::ast::Expr::RetainedLiteral(source)) = &column.default else {
        unreachable!()
    };
    assert!(!captured.0.value.shares_value_with(&source.0.value));
    assert_eq!(captured.0.ty, Some(crate::ColumnType::Text));
    assert_eq!(captured.0.declaration.as_deref(), Some("text"));
    assert_eq!(captured.0.collation_oid, Some(100));
    assert!(
        matches!(captured.0.value.resident_value(), Some(Value::Str(value)) if value.len() == 1 << 20)
    );
    drop(column);
    assert_eq!(original.used(), 0);
    assert!(memory.used() >= 1 << 20);
    drop(copied);
    assert_eq!(memory.used(), 0);
}

#[test]
fn definition_capture_rejects_query_expressions_outside_stored_column_invariants() {
    let source = table("CREATE TABLE invalid(id integer DEFAULT (SELECT 1))");
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    assert!(matches!(
        columns(&source.columns, &control),
        Err(SQLError::Internal(_))
    ));
    assert_eq!(memory.used(), 0);
}

#[test]
fn captured_columns_keep_nested_value_representations_and_catalog_identities() {
    let mut source = table("CREATE TABLE records(id integer GENERATED ALWAYS AS IDENTITY, payload text DEFAULT 'original' CHECK(length(payload)>0))");
    let values = [
        Value::Map(std::collections::BTreeMap::from([
            ("$uqa_type".into(), Value::Str("bytes".into())),
            ("data".into(), Value::Str("ordinary map".into())),
        ])),
        Value::Float(f64::from_bits(0xfff8_0000_0000_0007)),
    ];
    for (index, column) in source.columns.iter_mut().enumerate() {
        column.object_id = Some([u8::try_from(index + 1).unwrap(); 16]);
        column.attribute_number = Some(i16::try_from(index + 2).unwrap());
        column.missing_value = Some(values[index].clone());
    }
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let copied = columns(&source.columns, &control)
        .unwrap()
        .into_budgeted()
        .unwrap();
    source.columns.clear();
    drop(source);
    for (index, column) in copied.iter().enumerate() {
        assert_eq!(
            column.object_id,
            Some([u8::try_from(index + 1).unwrap(); 16])
        );
        assert_eq!(
            column.attribute_number,
            Some(i16::try_from(index + 2).unwrap())
        );
        assert!(column
            .missing_value
            .as_ref()
            .unwrap()
            .has_same_representation(&values[index]));
    }
    assert!(copied[0].auto_increment.is_some());
    assert!(copied[1].default.is_some());
    assert!(copied[1].check.is_some());
    assert!(memory.used() > 0);
    drop(copied);
    assert_eq!(memory.used(), 0);
}

#[test]
fn captured_definitions_preserve_constraints_partitions_and_dropped_slots() {
    let source = table("CREATE TABLE records(id integer, parent_id integer, CONSTRAINT positive CHECK(id>0), CONSTRAINT key PRIMARY KEY(id), CONSTRAINT reference FOREIGN KEY(parent_id) REFERENCES parents(id) ON DELETE SET NULL)");
    let partition = table("CREATE TABLE leaf PARTITION OF records FOR VALUES FROM (1) TO (9)");
    let dropped = crate::catalog::relation_attributes::DroppedAttribute::from_column(
        &source.columns[1],
        1,
        false,
        2,
    )
    .unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    macro_rules! same {
        ($copy:ident, $source:expr) => {{
            let source = $source;
            let copy = $copy(source, &control).unwrap().into_budgeted().unwrap();
            assert_eq!(
                serde_json::to_value(&*copy).unwrap(),
                serde_json::to_value(source).unwrap()
            );
            drop(copy);
            assert_eq!(memory.used(), 0);
        }};
    }
    same!(checks, &source.checks);
    same!(foreign_keys, &source.foreign_keys);
    same!(keys, &source.key_constraints);
    same!(hierarchy, &partition.hierarchy);
    same!(dropped_attributes, &[dropped]);
    same!(names, &["original".to_owned(), "other".to_owned()]);
    same!(optional_name, &Some("retained".to_owned()));
}

#[test]
fn definition_capture_admits_large_payloads_before_copying_and_releases_failed_output() {
    let column = crate::ast::ColumnDef::nullable("x".repeat(1 << 20), crate::ColumnType::Text);
    let memory = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            columns(std::slice::from_ref(&column), &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(allocation.bytes_max < 64 * 1024);
    assert_eq!(memory.used(), 0);
    let memory = MemoryBudget::new(2 << 20);
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let captured = columns(std::slice::from_ref(&column), &control)
        .unwrap()
        .into_budgeted()
        .unwrap();
    assert_eq!(captured[0].name.len(), 1 << 20);
    assert!(memory.used() >= 1 << 20);
    drop(captured);
    cancellation.cancel();
    assert!(matches!(
        columns(&[], &control),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}
