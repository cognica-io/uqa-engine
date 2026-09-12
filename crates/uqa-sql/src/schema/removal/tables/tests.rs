//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{Expr, Statement};
use std::cell::{Cell, RefCell};
fn declaration(sql: &str) -> crate::ast::CreateTable {
    let Statement::CreateTable(table) = crate::compile(sql).unwrap().remove(0) else {
        panic!("table declaration")
    };
    table
}
#[test]
fn inbound_foreign_key_detachment_preserves_unrelated_constraints_and_complete_column_metadata() {
    let table = declaration("CREATE TABLE child(source_id integer NOT NULL DEFAULT 7 REFERENCES public.parent(id), other_id integer REFERENCES other.parent(id), CONSTRAINT removed FOREIGN KEY(source_id) REFERENCES public.parent(id), CONSTRAINT retained FOREIGN KEY(other_id) REFERENCES other.parent(id))");
    let mut columns = table.columns;
    let mut foreign_keys = table.foreign_keys;
    let mut expected_columns = serde_json::to_value(&columns).unwrap();
    expected_columns[0]
        .as_object_mut()
        .unwrap()
        .remove("references");
    let retained = foreign_keys
        .iter()
        .find(|key| key.name.as_deref() == Some("retained"))
        .unwrap()
        .clone();
    assert!(detach_inbound_foreign_keys(
        &mut columns,
        &mut foreign_keys,
        &[RelationIdentity::new("public", "parent")]
    ));
    assert_eq!(serde_json::to_value(&columns).unwrap(), expected_columns);
    assert_eq!(foreign_keys, [retained]);
    let after = serde_json::to_value((&columns, &foreign_keys)).unwrap();
    assert!(!detach_inbound_foreign_keys(
        &mut columns,
        &mut foreign_keys,
        &[RelationIdentity::new("public", "parent")]
    ));
    assert_eq!(
        serde_json::to_value((&columns, &foreign_keys)).unwrap(),
        after
    );
}
struct Metadata {
    columns: RefCell<Vec<ColumnDef>>,
    checks: RefCell<Vec<TableCheck>>,
    check_reads: Cell<usize>,
}
impl TableRemovalMetadata for Metadata {
    fn columns(&self) -> TableColumnsRead<'_> {
        Box::new(self.columns.borrow())
    }
    fn table_checks(&self) -> TableChecksRead<'_> {
        self.check_reads.set(self.check_reads.get() + 1);
        Box::new(self.checks.borrow())
    }
    fn foreign_keys(&self) -> TableForeignKeysRead<'_> {
        panic!("schema reference inquiry must not read foreign keys")
    }
    fn key_constraints(&self) -> TableKeysRead<'_> {
        panic!("schema reference inquiry must not read keys")
    }
}
#[test]
fn schema_reference_inquiry_reads_table_checks_only_after_all_column_expressions_miss() {
    let table = declaration("CREATE TABLE child(value bigint CHECK(value > 0), CHECK(value < 10))");
    let metadata = Metadata {
        columns: RefCell::new(table.columns),
        checks: RefCell::new(table.checks),
        check_reads: Cell::new(0),
    };
    let reference = Expr::QualifiedColumn {
        qualifier: "public.parent".into(),
        column: "id".into(),
    };
    metadata.columns.borrow_mut()[0].default = Some(reference.clone());
    let target = RelationIdentity::new("public", "parent");
    assert!(table_schema_references_relation(&metadata, &target));
    assert_eq!(metadata.check_reads.get(), 0);
    metadata.columns.borrow_mut()[0].default = None;
    metadata.checks.borrow_mut()[0].expr = reference;
    assert!(table_schema_references_relation(&metadata, &target));
    assert_eq!(metadata.check_reads.get(), 1);
    assert!(metadata.columns.try_borrow_mut().is_ok());
    assert!(metadata.checks.try_borrow_mut().is_ok());
}
#[test]
fn ddl_table_binding_preserves_absent_targets_and_wrong_kind_diagnostics() {
    assert_eq!(resolved_table_ddl_target(None, "DROP TABLE").unwrap(), None);
    assert_eq!(
        resolved_table_ddl_target(Some(("tenant.items".into(), "table")), "ALTER TABLE").unwrap(),
        Some("tenant.items".into())
    );
    assert_eq!(
        resolved_table_ddl_target(Some(("tenant.items".into(), "view")), "DROP TABLE").unwrap_err(),
        "DROP TABLE: relation `tenant.items` is a view, not a table"
    );
}
