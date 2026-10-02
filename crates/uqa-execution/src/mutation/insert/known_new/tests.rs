//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Which prepared inserts skip the read of an existing row, and which the read of any earlier record.

use std::cell::RefCell;

use uqa_core::DocId;
use uqa_sql::{
    ast::{ColumnDef, ColumnType, ForeignKey, TableCheck, TableConstraintSet},
    catalog::index::EnforcedKey,
    semantics::conflict::ConflictCatalog,
};

use uqa_storage::mvcc::ObservedIdentifier;

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

/// Tables that reserve identities in their own durable namespace, recording each table the decision asks about.
#[derive(Default)]
struct Identifiers {
    asked: RefCell<Vec<String>>,
}

impl MutationIdentifiers for Identifiers {
    fn allocate_next_id(&self, _: &str) -> Result<DocId, SQLError> {
        unreachable!("the decision allocates nothing")
    }
    fn advance_next_id(&self, _: &str, _: DocId) -> uqa_storage::StorageBackendResult<()> {
        unreachable!("the decision allocates nothing")
    }
    fn persist_next_id(&self, _: &str) -> uqa_storage::StorageBackendResult<()> {
        unreachable!("the decision allocates nothing")
    }
    fn generates_unused_identities(&self, table: &str) -> Result<bool, SQLError> {
        self.asked.borrow_mut().push(table.to_owned());
        match table {
            "keyed" | "unkeyed" => Ok(true),
            "partition" => Ok(false),
            _ => Err(SQLError::UnknownTable(table.to_owned())),
        }
    }
}

fn raised_from(table: &'static str, previous: Option<u64>) -> ObservedIdentities {
    ObservedIdentities::found([(table, ObservedIdentifier::Observed { previous })])
}

#[test]
fn a_generated_identity_is_unused_where_its_table_reserves_it() {
    let catalog = Catalog::default();
    let identifiers = Identifiers::default();
    let observed = ObservedIdentities::default();
    let mut known_new = KnownNewInserts::new(&catalog, "id", false);
    for _ in 0..3 {
        assert_eq!(
            known_new
                .identity("unkeyed", &GENERATED, &observed, &identifiers)
                .unwrap(),
            InsertedIdentity::Unused
        );
        // An identity drawn from another table's namespace is only known to name no document now.
        assert_eq!(
            known_new
                .identity("partition", &GENERATED, &observed, &identifiers)
                .unwrap(),
            InsertedIdentity::Vacant
        );
    }
    // Each target table is asked once, however many rows it receives.
    assert_eq!(*identifiers.asked.borrow(), ["unkeyed", "partition"]);
    assert!(known_new
        .identity("missing", &GENERATED, &observed, &identifiers)
        .is_err());
}

#[test]
fn a_supplied_identity_is_unused_only_above_the_watermark_its_statement_found() {
    let catalog = Catalog::default();
    let identifiers = Identifiers::default();
    let mut known_new = KnownNewInserts::new(&catalog, "id", false);
    let mut identity = |observed: &ObservedIdentities| {
        known_new
            .identity("keyed", &SUPPLIED, observed, &identifiers)
            .unwrap()
    };
    assert_eq!(
        identity(&raised_from("keyed", Some(6))),
        InsertedIdentity::Unused
    );
    assert_eq!(
        identity(&raised_from("keyed", None)),
        InsertedIdentity::Unused
    );
    // At or below the earlier watermark a deleted document may have had the identity.
    assert_eq!(
        identity(&raised_from("keyed", Some(7))),
        InsertedIdentity::Vacant
    );
    assert_eq!(
        identity(&ObservedIdentities::found([(
            "keyed",
            ObservedIdentifier::Covered
        )])),
        InsertedIdentity::Vacant
    );
    assert_eq!(
        identity(&raised_from("other", Some(1))),
        InsertedIdentity::Vacant
    );
    // What a table reserves says nothing about an identity its statement supplied.
    assert!(identifiers.asked.borrow().is_empty());
}

#[test]
fn an_identity_that_may_name_a_row_is_unknown_whatever_the_watermark_shows() {
    let catalog = Catalog::default();
    let identifiers = Identifiers::default();
    let unused = raised_from("unkeyed", None);
    // Without a unique key two rows of the statement may supply the same identity, and the second replaces the first.
    let mut known_new = KnownNewInserts::new(&catalog, "id", false);
    assert_eq!(
        known_new
            .identity("unkeyed", &SUPPLIED, &unused, &identifiers)
            .unwrap(),
        InsertedIdentity::Unknown
    );
    // A statement that resolves conflicts may rewrite the row it meets.
    let mut resolving = KnownNewInserts::new(&catalog, "id", true);
    for prepared in [&SUPPLIED, &GENERATED] {
        assert_eq!(
            resolving
                .identity("keyed", prepared, &raised_from("keyed", None), &identifiers)
                .unwrap(),
            InsertedIdentity::Unknown
        );
    }
    assert!(identifiers.asked.borrow().is_empty());
}
