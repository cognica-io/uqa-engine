//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Which prepared inserts skip the read of an existing row.

use std::cell::RefCell;

use uqa_sql::{
    ast::{ColumnDef, ColumnType, ForeignKey, TableCheck, TableConstraintSet},
    catalog::index::EnforcedKey,
    semantics::conflict::ConflictCatalog,
};

use super::*;

/// Unique scalar columns by table, recording each table the decision asks about.
#[derive(Default)]
struct Catalog {
    asked: RefCell<Vec<String>>,
}

impl ConflictCatalog for Catalog {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        unreachable!("the decision reads only unique columns")
    }
    fn enforced_keys(&self, _: &str) -> Result<Vec<EnforcedKey>, String> {
        unreachable!("the decision reads only unique columns")
    }
    fn try_declared_table_constraints(&self, _: &str) -> Result<TableConstraintSet, String> {
        unreachable!("the decision reads only unique columns")
    }
}

impl ConstraintCatalog for Catalog {
    fn try_unique_columns(&self, table: &str) -> Result<Vec<String>, String> {
        self.asked.borrow_mut().push(table.to_owned());
        match table {
            "keyed" => Ok(vec!["code".into(), "id".into()]),
            "unkeyed" => Ok(vec!["code".into()]),
            _ => Err(format!("unknown table {table}")),
        }
    }
    fn try_check_constraint_definitions(&self, _: &str) -> Result<Vec<TableCheck>, String> {
        unreachable!("the decision reads only unique columns")
    }
    fn try_foreign_keys(&self, _: &str) -> Result<Vec<ForeignKey>, String> {
        unreachable!("the decision reads only unique columns")
    }
    fn column_type(&self, _: &str, _: &str) -> Result<Option<ColumnType>, String> {
        unreachable!("the decision reads only unique columns")
    }
    fn hierarchy_scan_tables(&self, _: &str, _: bool) -> Result<Vec<String>, SQLError> {
        unreachable!("the decision reads only unique columns")
    }
}

const SUPPLIED: PreparedInsertConflict = PreparedInsertConflict::Insert {
    doc_id: 7,
    supplied: true,
};
const GENERATED: PreparedInsertConflict = PreparedInsertConflict::Insert {
    doc_id: 7,
    supplied: false,
};

#[test]
fn a_generated_identity_is_new_without_a_catalog_read() {
    let catalog = Catalog::default();
    let mut known_new = KnownNewInserts::new(&catalog, "id", false);
    assert!(known_new.contains("unkeyed", &GENERATED).unwrap());
    assert!(known_new.contains("keyed", &GENERATED).unwrap());
    assert!(catalog.asked.borrow().is_empty());
}

#[test]
fn a_supplied_identity_is_new_only_when_it_is_a_unique_key() {
    let catalog = Catalog::default();
    let mut known_new = KnownNewInserts::new(&catalog, "id", false);
    for _ in 0..3 {
        assert!(known_new.contains("keyed", &SUPPLIED).unwrap());
        // Without a unique key the identity may name a row the insert replaces.
        assert!(!known_new.contains("unkeyed", &SUPPLIED).unwrap());
    }
    // Each target table is read once, however many rows it receives.
    assert_eq!(*catalog.asked.borrow(), ["keyed", "unkeyed"]);
    assert!(known_new.contains("missing", &SUPPLIED).is_err());
}

#[test]
fn a_statement_that_resolves_conflicts_knows_no_insert_to_be_new() {
    let catalog = Catalog::default();
    let mut known_new = KnownNewInserts::new(&catalog, "id", true);
    assert!(!known_new.contains("keyed", &SUPPLIED).unwrap());
    assert!(!known_new.contains("keyed", &GENERATED).unwrap());
    assert!(catalog.asked.borrow().is_empty());
}
