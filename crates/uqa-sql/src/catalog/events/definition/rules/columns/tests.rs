//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnDef, Expr},
    catalog::events::definition::tests::fixtures::Catalog,
    semantics::returning::ReturningCatalog,
    ColumnType, RowSchema,
};
use std::cell::Cell;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

struct Rows<'a> {
    columns: Vec<ColumnDef>,
    fail: Cell<bool>,
    unwind: Cell<bool>,
    interrupt: Option<&'a CancellationToken>,
}
impl Rows<'_> {
    fn new(columns: Vec<ColumnDef>) -> Self {
        Self {
            columns,
            fail: Cell::new(false),
            unwind: Cell::new(false),
            interrupt: None,
        }
    }
}
impl ReturningCatalog for Rows<'_> {
    fn try_describe_table_row_type(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        panic!("controlled rule names must borrow their row declaration")
    }
    fn with_table_row_type_with_control(
        &self,
        _: &str,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(&[ColumnDef]) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        read(&self.columns)?;
        assert!(!self.unwind.get(), "catalog unwind");
        if let Some(token) = self.interrupt {
            token.cancel();
        }
        control.check()?;
        if self.fail.get() {
            return Err(SQLError::Internal("catalog unavailable".into()));
        }
        Ok(true)
    }
    fn try_table_columns(&self, _: &str) -> Result<Vec<String>, String> {
        panic!("names already belong to the row declaration")
    }
    fn view_schema(&self, _: &str) -> Result<Option<RowSchema>, SQLError> {
        panic!("event views use their current restored definition")
    }
}

#[test]
fn rule_target_names_borrow_defaults_and_keep_exact_destination_leases() {
    let mut column = ColumnDef::nullable("first", ColumnType::Text);
    column.default = Some(Expr::Literal(Value::Str("x".repeat(4 * 1024 * 1024))));
    let rows = Rows::new(vec![
        column,
        ColumnDef::nullable("second", ColumnType::Integer),
    ]);
    let catalog = Catalog::default();
    let mut context = catalog.context();
    context.returning.catalog = &rows;
    let memory = MemoryBudget::new(1024);
    let sibling = memory.reserve(37).unwrap();
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(
            context
                .rule_relation_column_names_with_control("items", &control)
                .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(&*output, &["first", "second"]);
    assert!(measured.bytes_total < 1024, "{measured:?}");
    assert_eq!(
        usize::try_from(measured.bytes_current).unwrap(),
        memory.used() - sibling.bytes()
    );
    assert_eq!(
        memory.used() - sibling.bytes(),
        output.capacity() * size_of::<String>()
            + output.iter().map(String::capacity).sum::<usize>()
    );
    drop(output);
    assert_eq!(memory.used(), 37);
    drop(sibling);
    assert_eq!(memory.used(), 0);
}

#[test]
fn wide_rule_target_names_fail_before_copy_and_release_prior_columns() {
    let rows = Rows::new(vec![
        ColumnDef::nullable("first", ColumnType::Integer),
        ColumnDef::nullable("x".repeat(4 * 1024 * 1024), ColumnType::Integer),
    ]);
    let catalog = Catalog::default();
    let mut context = catalog.context();
    context.returning.catalog = &rows;
    let memory = MemoryBudget::new(512);
    let sibling = memory.reserve(37).unwrap();
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let measured = allocation_counter::measure(|| {
        assert_eq!(
            context
                .rule_relation_column_names_with_control("items", &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(measured.bytes_total < 16 * 1024, "{measured:?}");
    assert_eq!(memory.used(), sibling.bytes());
}

#[test]
fn target_catalog_failure_cancellation_and_unwind_release_partial_names() {
    let memory = MemoryBudget::new(1024);
    let sibling = memory.reserve(37).unwrap();
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let catalog = Catalog::default();
    let mut rows = Rows::new(vec![ColumnDef::nullable("n", ColumnType::Integer)]);
    for token in [&original, &invoking] {
        rows.interrupt = Some(token);
        let mut context = catalog.context();
        context.returning.catalog = &rows;
        assert_eq!(
            context
                .rule_relation_column_names_with_control("items", &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 37);
        token.reset();
    }
    rows.interrupt = None;
    let mut context = catalog.context();
    context.returning.catalog = &rows;
    rows.fail.set(true);
    assert_eq!(
        context
            .rule_relation_column_names_with_control("items", &control)
            .unwrap_err()
            .to_string(),
        "internal error: read rule columns: catalog unavailable"
    );
    assert_eq!(memory.used(), 37);
    rows.fail.set(false);
    rows.unwind.set(true);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        context.rule_relation_column_names_with_control("items", &control)
    }))
    .is_err());
    assert_eq!(memory.used(), 37);
    rows.unwind.set(false);
    assert_eq!(
        &*context
            .rule_relation_column_names_with_control("items", &control)
            .unwrap(),
        &["n"]
    );
    assert_eq!(memory.used(), 37);
    let empty = Rows::new(Vec::new());
    context.returning.catalog = &empty;
    assert!(context
        .rule_relation_column_names_with_control("items", &control)
        .unwrap()
        .is_empty());
    assert_eq!(memory.used(), 37);
    drop(sibling);
    assert_eq!(memory.used(), 0);
}
