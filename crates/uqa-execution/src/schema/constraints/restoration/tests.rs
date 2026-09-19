//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::RelationIdentity;
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore, RelationSecurityRow};

fn catalog() -> KeyValueCatalog {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    catalog
        .save_schema_row(&uqa_storage::SchemaRow::bootstrap("public"))
        .unwrap();
    catalog
}

fn columns() -> Vec<uqa_sql::ast::ColumnDef> {
    let uqa_sql::Statement::CreateTable(table) =
        uqa_sql::compile("CREATE TABLE t(v integer CONSTRAINT required NOT NULL)")
            .unwrap()
            .remove(0)
    else {
        panic!("table declaration")
    };
    table.columns
}

fn table(name: &str) -> TableSchema {
    TableSchema {
        relation: RelationIdentity::new("public", name),
        security: RelationSecurityRow::legacy("uqa"),
        object_id: [1; 16],
        storage_generation: [2; 16],
        analyzer_json: "{}".into(),
        fts_fields: Vec::new(),
        vector_fields: Vec::new(),
        columns_json: serde_json::to_string(&columns()).unwrap(),
        constraints_json: serde_json::to_string(&uqa_sql::ast::TableConstraintSet::default())
            .unwrap(),
    }
}

#[test]
fn migration_preserves_legacy_oids_for_tables_and_foreign_tables_and_then_is_read_only() {
    let catalog = catalog();
    catalog.save_table(&table("ordinary")).unwrap();
    catalog
        .save_foreign_table(&uqa_storage::ForeignTableRow {
            relation: RelationIdentity::new("public", "foreign_relation"),
            security: RelationSecurityRow::legacy("uqa"),
            server_name: "memory".into(),
            columns_json: serde_json::to_string(&columns()).unwrap(),
            options_json: "{}".into(),
        })
        .unwrap();
    migrate_constraint_catalog(&catalog).unwrap();
    validate_constraint_catalog(&catalog).unwrap();
    let first = catalog.load_tables().unwrap()[0].columns_json.clone();
    let ordinary: Vec<uqa_sql::ast::ColumnDef> = serde_json::from_str(&first).unwrap();
    let row = catalog.load_foreign_tables().unwrap().remove(0);
    let (foreign, _) = crate::catalog::foreign::StoredForeignTable::from_catalog(
        row.relation.qualified_name(),
        row.server_name.clone(),
        BTreeMap::new(),
        &row.columns_json,
    )
    .unwrap();
    for (columns, relation) in [
        (&ordinary, "ordinary"),
        (&foreign.columns, "foreign_relation"),
    ] {
        assert_eq!(
            columns[0].not_null_identity.unwrap().oid,
            uqa_sql::catalog::oids::stable_oid(
                "constraint",
                &format!("public.{relation}.required")
            )
        );
    }
    assert_ne!(
        ordinary[0].not_null_identity.unwrap().object_id,
        foreign.columns[0].not_null_identity.unwrap().object_id
    );
    migrate_constraint_catalog(&catalog).unwrap();
    assert_eq!(catalog.load_tables().unwrap()[0].columns_json, first);
    assert_eq!(
        catalog.load_foreign_tables().unwrap()[0].columns_json,
        row.columns_json
    );
    assert_eq!(
        catalog
            .get_metadata(IDENTITY_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some("1")
    );
}

#[test]
fn missing_current_constraint_identity_is_rejected_without_repair() {
    let catalog = catalog();
    let original = table("t");
    catalog.save_table(&original).unwrap();
    catalog.set_metadata(IDENTITY_METADATA_KEY, "1").unwrap();
    assert!(migrate_constraint_catalog(&catalog)
        .unwrap_err()
        .to_string()
        .contains("initial catalog identity migration"));
    assert_eq!(
        catalog.load_tables().unwrap()[0].columns_json,
        original.columns_json
    );
}

#[test]
fn duplicate_identities_reject_the_entire_migration_before_writes() {
    for duplicate_oid in [false, true] {
        let catalog = catalog();
        let mut first = table("first");
        let mut second = table("second");
        let mut first_columns = columns();
        let mut second_columns = columns();
        first_columns[0].not_null_identity = Some(uqa_sql::ast::ConstraintCatalogIdentity {
            object_id: [1; 16],
            oid: 21_000,
        });
        second_columns[0].not_null_identity = Some(uqa_sql::ast::ConstraintCatalogIdentity {
            object_id: if duplicate_oid { [2; 16] } else { [1; 16] },
            oid: if duplicate_oid { 21_000 } else { 21_001 },
        });
        first.columns_json = serde_json::to_string(&first_columns).unwrap();
        second.columns_json = serde_json::to_string(&second_columns).unwrap();
        catalog.save_table(&first).unwrap();
        catalog.save_table(&second).unwrap();
        assert!(migrate_constraint_catalog(&catalog)
            .unwrap_err()
            .to_string()
            .contains("duplicate NOT NULL"));
        let current = catalog.load_tables().unwrap();
        assert_eq!(
            current
                .iter()
                .find(|table| table.relation.name == "first")
                .unwrap()
                .columns_json,
            first.columns_json
        );
        assert!(catalog
            .get_metadata(IDENTITY_METADATA_KEY)
            .unwrap()
            .is_none());
    }
}

#[test]
fn malformed_foreign_identity_rejects_ordinary_migration_before_writes() {
    let catalog = catalog();
    let ordinary = table("ordinary");
    catalog.save_table(&ordinary).unwrap();
    let mut invalid = columns();
    invalid[0].not_null_identity = Some(uqa_sql::ast::ConstraintCatalogIdentity {
        object_id: [0; 16],
        oid: 21_000,
    });
    catalog
        .save_foreign_table(&uqa_storage::ForeignTableRow {
            relation: RelationIdentity::new("public", "foreign_relation"),
            security: RelationSecurityRow::legacy("uqa"),
            server_name: "memory".into(),
            columns_json: serde_json::to_string(&invalid).unwrap(),
            options_json: "{}".into(),
        })
        .unwrap();
    assert!(migrate_constraint_catalog(&catalog)
        .unwrap_err()
        .to_string()
        .contains("invalid NOT NULL"));
    assert_eq!(
        catalog.load_tables().unwrap()[0].columns_json,
        ordinary.columns_json
    );
    assert!(catalog
        .get_metadata(IDENTITY_METADATA_KEY)
        .unwrap()
        .is_none());
}

#[test]
fn load_only_validation_requires_the_marker_and_rejects_cross_relation_duplicates() {
    let catalog = catalog();
    let original = table("ordinary");
    catalog.save_table(&original).unwrap();
    assert!(validate_constraint_catalog(&catalog)
        .unwrap_err()
        .to_string()
        .contains("initial catalog identity migration"));
    assert_eq!(
        catalog.load_tables().unwrap()[0].columns_json,
        original.columns_json
    );
    migrate_constraint_catalog(&catalog).unwrap();
    let materialized = catalog.load_tables().unwrap().remove(0);
    catalog
        .save_foreign_table(&uqa_storage::ForeignTableRow {
            relation: RelationIdentity::new("public", "duplicate"),
            security: RelationSecurityRow::legacy("uqa"),
            server_name: "memory".into(),
            columns_json: materialized.columns_json.clone(),
            options_json: "{}".into(),
        })
        .unwrap();
    assert!(validate_constraint_catalog(&catalog)
        .unwrap_err()
        .to_string()
        .contains("duplicate NOT NULL"));
    catalog.set_metadata(IDENTITY_METADATA_KEY, "2").unwrap();
    assert!(validate_constraint_catalog(&catalog)
        .unwrap_err()
        .to_string()
        .contains("unknown NOT NULL"));
    assert_eq!(
        catalog
            .get_metadata(IDENTITY_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some("2")
    );
}

mod catalog_addresses;
mod foreign_key_identities;
mod hierarchy;
