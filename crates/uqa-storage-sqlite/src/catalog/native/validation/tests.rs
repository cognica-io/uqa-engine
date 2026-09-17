//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Namespace validation must inspect private and retained records without hydrating index payloads.

use super::*;
use crate::{Catalog, ManagedConnection};
use uqa_storage::{
    catalog::{ForeignTableRow, ViewRow},
    mvcc::VersionedSessionOptions,
    CatalogFacade, RelationIdentity, SequenceOptions, SequenceRow, TableSchema,
};

fn text(value: &str) -> ValueRef<'_> {
    ValueRef::Text(value.as_bytes())
}

#[test]
fn private_native_sequence_provenance_follows_generation_replacement_and_undo() {
    let (connection, catalog) = fixture();
    let mut row = catalog.load_sequence_rows().unwrap().remove(0);
    assert!(!catalog
        .sequence_has_private_changes(&row.relation, row.object_id)
        .unwrap());
    connection.begin_transaction().unwrap();
    connection.savepoint("before_definition").unwrap();
    row.definition_generation = [9; 16];
    catalog.replace_sequence_row(&row).unwrap();
    assert!(catalog
        .sequence_has_private_changes(&row.relation, row.object_id)
        .unwrap());
    connection
        .rollback_to_savepoint("before_definition")
        .unwrap();
    assert!(!catalog
        .sequence_has_private_changes(&row.relation, row.object_id)
        .unwrap());
    catalog
        .rename_sequence_row("app.ids", "app.renamed_ids")
        .unwrap();
    row.relation.name = "renamed_ids".into();
    assert!(catalog
        .sequence_has_private_changes(&row.relation, row.object_id)
        .unwrap());
    connection.rollback_transaction().unwrap();
    assert!(!catalog
        .sequence_has_private_changes(&row.relation, row.object_id)
        .unwrap());
}

fn fixture() -> (ManagedConnection, Catalog) {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_schema("app").unwrap();
    catalog
        .save_table(&TableSchema {
            relation: RelationIdentity::new("app", "docs"),
            role_owner: "uqa".into(),
            acl: None,
            column_acls: std::collections::BTreeMap::default(),
            object_id: [1; 16],
            storage_generation: [2; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    catalog
        .save_view(&ViewRow {
            relation: RelationIdentity::new("app", "visible"),
            role_owner: "uqa".into(),
            acl: None,
            column_acls: std::collections::BTreeMap::default(),
            definition_json: "{}".into(),
        })
        .unwrap();
    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("app", "ids"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [3; 16],
            definition_generation: [4; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: None,
        })
        .unwrap();
    catalog
        .save_foreign_table(&ForeignTableRow {
            relation: RelationIdentity::new("app", "remote"),
            role_owner: "uqa".into(),
            acl: None,
            column_acls: std::collections::BTreeMap::default(),
            server_name: "remote".into(),
            columns_json: "[]".into(),
            options_json: "{}".into(),
        })
        .unwrap();
    catalog
        .save_catalog_index(
            &RelationIdentity::new("app", "docs_idx"),
            "btree",
            "app.docs",
            "[\"id\"]",
            "{}",
        )
        .unwrap();
    catalog
        .save_model("unrelated", &"x".repeat(128 * 1024))
        .unwrap();
    connection.with(|raw| {
        raw.execute("WITH RECURSIVE ids(id) AS (VALUES (1) UNION ALL SELECT id + 1 FROM ids WHERE id < 70) INSERT INTO _documents(table_name, doc_id, body) SELECT 'app.docs', id, '{}' FROM ids", [])?;
        for field in [rusqlite::types::Value::Text("id".into()), rusqlite::types::Value::Blob(b"id".to_vec())] {
            raw.execute("INSERT INTO _btree_indexes VALUES ('app.docs', ?1)", [&field])?;
            raw.execute(
                "WITH RECURSIVE ids(id) AS (VALUES (1) UNION ALL SELECT id + 1 FROM ids WHERE id < 70) INSERT INTO _btree_index_entries SELECT 'app.docs', ?1, id, CASE WHEN id = 70 THEN ?2 ELSE '1' END FROM ids",
                rusqlite::params![field, format!("\"{}\"", "x".repeat(64 * 1024))],
            )?;
        }
        Ok(())
    }).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    (connection, catalog)
}

#[test]
fn private_catalog_corruption_is_rejected_without_consulting_valid_physical_rows() {
    let (connection, catalog) = fixture();
    catalog.migrate_relation_namespace().unwrap();
    for (fault, expected) in [
        ("schema", "missing schema"),
        ("claim", "definition has no matching catalog relation"),
        ("kind", "definition has no matching catalog relation"),
        ("unknown kind", "relation has an unknown kind"),
        (
            "definition kind",
            "definition kind disagrees with its record family",
        ),
        (
            "duplicate",
            "multiple native definitions claim the same relation name",
        ),
        ("orphan", "relation has no matching definition"),
        ("index target", "index references a missing table"),
        ("column index", "B-tree entry references a missing index"),
        ("named index", "B-tree entry references a missing index"),
    ] {
        connection.begin_transaction().unwrap();
        corrupt(&connection, fault);
        let error = catalog.migrate_relation_namespace().unwrap_err();
        assert!(error.to_string().contains(expected), "{fault}: {error}");
        connection.rollback_transaction().unwrap();
        catalog.migrate_relation_namespace().unwrap();
    }
}

fn corrupt(connection: &ManagedConnection, fault: &str) {
    connection
        .with_native_write(|snapshot, batch| {
            let database = NativeRecordOwner::Database(snapshot.database);
            let table = snapshot.table_owner("app.docs")?.unwrap();
            match fault {
                "schema" => {
                    snapshot.delete_prefix(batch, Family::Schemas, database, &[text("app")])?;
                }
                "claim" => snapshot.delete_prefix(
                    batch,
                    Family::Relations,
                    database,
                    &[text("app"), text("docs")],
                )?,
                "kind" => snapshot.put_row(
                    batch,
                    Family::Relations,
                    database,
                    &[text("app"), text("docs"), text("view")],
                )?,
                "unknown kind" => snapshot.put_row(
                    batch,
                    Family::Relations,
                    database,
                    &[text("app"), text("docs"), text("unknown")],
                )?,
                "definition kind" => {
                    snapshot
                        .read_row(Family::Tables, table, &[], |row| {
                            let mut row = row.to_vec();
                            row[2] = text("view");
                            snapshot.put_row(batch, Family::Tables, table, &row)
                        })?
                        .unwrap();
                }
                "duplicate" => {
                    snapshot
                        .read_row(
                            Family::Views,
                            database,
                            &[text("app"), text("visible")],
                            |row| {
                                let mut row = row.to_vec();
                                row[1] = text("docs");
                                snapshot.put_row(batch, Family::Views, database, &row)
                            },
                        )?
                        .unwrap();
                }
                "orphan" => snapshot.put_row(
                    batch,
                    Family::Relations,
                    database,
                    &[text("app"), text("orphan"), text("table")],
                )?,
                "index target" => {
                    snapshot.delete_prefix(batch, Family::Tables, table, &[])?;
                    snapshot.delete_prefix(
                        batch,
                        Family::Relations,
                        database,
                        &[text("app"), text("docs")],
                    )?;
                }
                "column index" => {
                    snapshot.delete_prefix(batch, Family::BtreeIndexes, table, &[text("id")])?;
                }
                "named index" => snapshot.delete_prefix(
                    batch,
                    Family::BtreeIndexes,
                    table,
                    &[ValueRef::Blob(b"id")],
                )?,
                _ => unreachable!(),
            }
            Ok(())
        })
        .unwrap()
        .unwrap();
}

#[test]
fn validation_holds_its_original_view_after_rollback_and_sibling_changes() {
    let (connection, catalog) = fixture();
    connection.begin_transaction().unwrap();
    connection
        .with_native_write(|snapshot, batch| {
            snapshot.delete_prefix(
                batch,
                Family::Relations,
                NativeRecordOwner::Database(snapshot.database),
                &[text("app"), text("visible")],
            )
        })
        .unwrap()
        .unwrap();
    let invalid = connection.native_snapshot().unwrap().unwrap();
    connection.rollback_transaction().unwrap();
    catalog.migrate_relation_namespace().unwrap();
    assert!(invalid.validate_catalog_namespace().is_err());

    let valid = connection.native_snapshot().unwrap().unwrap();
    let sibling = Catalog::open(connection.new_session()).unwrap();
    sibling
        .drop_view(&RelationIdentity::new("app", "visible"))
        .unwrap();
    valid.validate_catalog_namespace().unwrap();
    sibling.migrate_relation_namespace().unwrap();
    assert_eq!(catalog.load_views().unwrap().len(), 0);
}

#[test]
fn validation_pages_past_deleted_entries_without_loading_large_values() {
    let (connection, _) = fixture();
    connection
        .with_native_write(|snapshot, batch| {
            let owner = snapshot.table_owner("app.docs")?.unwrap();
            for id in 1..=65 {
                snapshot.delete_prefix(
                    batch,
                    Family::BtreeIndexEntries,
                    owner,
                    &[text("id"), ValueRef::Integer(id)],
                )?;
            }
            Ok(())
        })
        .unwrap()
        .unwrap();
    let mut snapshot = std::sync::Arc::try_unwrap(connection.native_snapshot().unwrap().unwrap())
        .ok()
        .unwrap();
    snapshot.control = StorageReadControl::with_limit(32 * 1024);
    snapshot.validate_catalog_namespace().unwrap();
    assert_eq!(snapshot.control.memory().used(), 0);
    snapshot.control = StorageReadControl::with_limit(1);
    assert!(snapshot.validate_catalog_namespace().is_err());
    assert_eq!(snapshot.control.memory().used(), 0);
    snapshot.control = StorageReadControl::with_limit(32 * 1024);
    snapshot.control.cancellation().cancel();
    assert!(snapshot.validate_catalog_namespace().is_err());
    assert_eq!(snapshot.control.memory().used(), 0);
}
