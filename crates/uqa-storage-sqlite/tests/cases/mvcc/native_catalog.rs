//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native catalog registry operations use the same logical snapshot and publication boundary as documents.

use super::{open, MODES};
use std::{collections::BTreeMap, sync::mpsc, time::Duration};
use uqa_core::{
    catalog_role::{BoundAclEntry, RoleIdentity},
    catalog_schema::BoundSchemaRow,
    Value,
};
use uqa_storage::{
    mvcc::VersionedSessionOptions, DocumentStore, RelationIdentity, SchemaPrivileges, SchemaRow,
    TableSchema,
};
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteDocumentStore};

fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}

fn memory() -> (ManagedConnection, Catalog) {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    bind(&connection);
    (connection, catalog)
}

fn write(catalog: &Catalog, name: &str, value: &str) {
    catalog.set_metadata(name, value).unwrap();
    catalog.save_model(name, value).unwrap();
    catalog.save_scoring_params(name, value).unwrap();
    catalog.save_analyzer_revision(name, value, value).unwrap();
    catalog
        .replace_table_field_analyzer_binding("docs", name, "both", name, value)
        .unwrap();
    catalog.save_schema(name).unwrap();
}

fn assert_value(catalog: &Catalog, name: &str, value: Option<&str>) {
    assert_eq!(catalog.get_metadata(name).unwrap().as_deref(), value);
    assert_eq!(catalog.load_model(name).unwrap().as_deref(), value);
    assert_eq!(catalog.load_scoring_params(name).unwrap().as_deref(), value);
    let descriptors = catalog.load_analyzer_descriptors().unwrap();
    assert_eq!(
        descriptors
            .iter()
            .find(|(found, _)| found == name)
            .map(|(_, value)| value.as_str()),
        value
    );
    let fields = catalog.load_table_field_analyzer_bindings().unwrap();
    assert_eq!(
        fields
            .iter()
            .find(|(table, field, _)| table == "docs" && field == name)
            .map(|(_, _, value)| value.as_str()),
        value
    );
    assert_eq!(
        catalog
            .load_schemas()
            .unwrap()
            .iter()
            .any(|found| found == name),
        value.is_some()
    );
}

#[test]
fn independent_native_catalog_writers_commit_before_the_other_private_transaction_ends() {
    for mode in MODES {
        for ending in ["commit", "rollback", "savepoint"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("catalog.db");
            let connection = open(mode, &path);
            let catalog = Catalog::open(connection.clone()).unwrap();
            catalog
                .save_table_field_analyzer("docs", "seed", "both", "standard")
                .unwrap();
            bind(&connection);
            connection.begin_transaction().unwrap();
            write(&catalog, "a", "{\"value\":1}");
            connection.savepoint("keep").unwrap();
            write(&catalog, "a", "{\"value\":2}");
            let other_path = path.clone();
            let (sent, received) = mpsc::channel();
            let writer = std::thread::spawn(move || {
                let connection = open(mode, &other_path);
                bind(&connection);
                let catalog = Catalog::open(connection.clone()).unwrap();
                connection.begin_transaction().unwrap();
                write(&catalog, "b", "{\"value\":3}");
                connection.commit_transaction().unwrap();
                sent.send(()).unwrap();
            });
            let completed = received.recv_timeout(Duration::from_secs(20));
            if completed.is_err() {
                connection.rollback_transaction().unwrap();
                writer.join().unwrap();
                panic!("native catalog writer did not finish: {mode:?} {completed:?}");
            }
            writer.join().unwrap();
            assert!(connection.in_transaction());
            assert_value(&catalog, "a", Some("{\"value\":2}"));
            assert_value(&catalog, "b", None);
            // Attaching another catalog handle must preserve the existing transaction.
            assert_value(&Catalog::open(connection.clone()).unwrap(), "b", None);
            let expected = match ending {
                "commit" => {
                    connection.commit_transaction().unwrap();
                    Some("{\"value\":2}")
                }
                "rollback" => {
                    connection.rollback_transaction().unwrap();
                    None
                }
                _ => {
                    connection.rollback_to_savepoint("keep").unwrap();
                    connection.commit_transaction().unwrap();
                    Some("{\"value\":1}")
                }
            };
            drop(catalog);
            drop(connection);
            let reopened = open(mode, &path);
            bind(&reopened);
            let catalog = Catalog::open(reopened).unwrap();
            assert_value(&catalog, "a", expected);
            assert_value(&catalog, "b", Some("{\"value\":3}"));
        }
    }
}

#[test]
fn native_named_registries_keep_sorted_values_and_transactional_deletions() {
    let (connection, catalog) = memory();
    for name in ["z", "a", "a\0suffix"] {
        catalog.save_model(name, name).unwrap();
        catalog.save_scoring_params(name, name).unwrap();
        catalog.save_analyzer_revision(name, name, name).unwrap();
    }
    let expected = vec![
        ("a".into(), "a".into()),
        ("a\0suffix".into(), "a\0suffix".into()),
        ("z".into(), "z".into()),
    ];
    assert_eq!(catalog.load_models().unwrap(), expected);
    assert_eq!(catalog.load_all_scoring_params().unwrap(), expected);
    assert_eq!(catalog.load_analyzers().unwrap(), expected);
    assert_eq!(catalog.load_analyzer_descriptors().unwrap(), expected);
    catalog.save_analyzer("a", "legacy").unwrap();
    assert_eq!(catalog.load_analyzer_descriptors().unwrap().len(), 2);
    connection.begin_transaction().unwrap();
    catalog.drop_model("a").unwrap();
    catalog.drop_scoring_params("a").unwrap();
    catalog.drop_analyzer("a").unwrap();
    assert!(catalog.load_model("a").unwrap().is_none());
    assert!(catalog.load_scoring_params("a").unwrap().is_none());
    assert_eq!(catalog.load_analyzers().unwrap().len(), 2);
    connection.rollback_transaction().unwrap();
    assert_eq!(catalog.load_models().unwrap(), expected);
    catalog.drop_model("z").unwrap();
    catalog.drop_scoring_params("z").unwrap();
    catalog.drop_analyzer("z").unwrap();
    assert_eq!(catalog.load_models().unwrap().len(), 2);
    assert_eq!(catalog.load_all_scoring_params().unwrap().len(), 2);
    assert_eq!(catalog.load_analyzers().unwrap().len(), 2);
}

#[test]
fn native_analyzer_binding_replacement_and_legacy_writes_preserve_other_phases() {
    let (connection, catalog) = memory();
    catalog
        .replace_table_field_analyzer_binding("z", "text", "index", "revision", "{\"revision\":1}")
        .unwrap();
    catalog
        .save_table_field_analyzer("z", "text", "search", "searcher")
        .unwrap();
    assert!(catalog
        .load_table_field_analyzer_bindings()
        .unwrap()
        .is_empty());
    assert_eq!(
        catalog.load_table_field_analyzers().unwrap(),
        vec![
            ("z".into(), "text".into(), "index".into(), "revision".into()),
            (
                "z".into(),
                "text".into(),
                "search".into(),
                "searcher".into()
            )
        ]
    );
    catalog
        .replace_table_field_analyzer_binding("a", "body", "both", "first", "binding")
        .unwrap();
    connection.begin_transaction().unwrap();
    catalog
        .replace_table_field_analyzer("z", "text", "both", "combined")
        .unwrap();
    catalog
        .drop_table_field_analyzer_field("a", "body")
        .unwrap();
    assert_eq!(
        catalog.load_table_field_analyzers().unwrap(),
        vec![("z".into(), "text".into(), "both".into(), "combined".into())]
    );
    connection.rollback_transaction().unwrap();
    assert_eq!(catalog.load_table_field_analyzers().unwrap().len(), 3);
    assert_eq!(catalog.load_table_field_analyzers().unwrap()[0].0, "a");
    catalog
        .replace_table_field_analyzer_binding("z", "text", "both", "new", "binding2")
        .unwrap();
    assert_eq!(
        catalog.load_table_field_analyzer_bindings().unwrap(),
        vec![
            ("a".into(), "body".into(), "binding".into()),
            ("z".into(), "text".into(), "binding2".into())
        ]
    );
    catalog.drop_table_field_analyzers("z").unwrap();
    assert_eq!(catalog.load_table_field_analyzers().unwrap().len(), 1);
}

#[test]
fn native_catalog_schema_ownership_and_format_version_survive_failed_operations() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_schema("occupied").unwrap();
    catalog
        .save_table(&TableSchema {
            relation: RelationIdentity::new("occupied", "existing"),
            security: uqa_storage::RelationSecurityRow::legacy("uqa"),
            object_id: [1; 16],
            storage_generation: [2; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: "[]".into(),
        })
        .unwrap();
    bind(&connection);
    let owner = RoleIdentity {
        oid: 20_001,
        object_id: [1; 16],
    };
    let reader = RoleIdentity {
        oid: 20_002,
        object_id: [2; 16],
    };
    let schema = SchemaRow::Bound(BoundSchemaRow {
        name: "private".into(),
        role_owner: owner,
        acl: Some(vec![BoundAclEntry {
            role: Some(reader),
            grantor: owner,
            privileges: SchemaPrivileges {
                usage: true,
                create: false,
            },
            grant_options: SchemaPrivileges::default(),
        }]),
    });
    catalog.save_schema_row(&schema).unwrap();
    assert_eq!(
        catalog
            .load_schema_rows()
            .unwrap()
            .into_iter()
            .find(|row| row.name() == "private"),
        Some(schema)
    );
    assert!(catalog.drop_schema("occupied").is_err());
    assert!(!connection.in_transaction());
    assert!(catalog.load_schemas().unwrap().contains(&"occupied".into()));
    connection.begin_transaction().unwrap();
    assert!(catalog.set_metadata("schema_version", "48").is_err());
    assert_eq!(
        catalog.get_metadata("schema_version").unwrap().as_deref(),
        Some("49")
    );
    catalog.set_metadata("custom", "valid").unwrap();
    connection.commit_transaction().unwrap();
    catalog.drop_schema("private").unwrap();
    assert!(!catalog.load_schemas().unwrap().contains(&"private".into()));
    assert_eq!(
        catalog.get_metadata("custom").unwrap().as_deref(),
        Some("valid")
    );
}

#[test]
fn native_catalog_same_record_conflicts_preserve_the_other_commit() {
    let (connection, catalog) = memory();
    catalog.save_model("same", "original").unwrap();
    let other = connection.new_session();
    let b = Catalog::open(other.clone()).unwrap();
    connection.begin_transaction().unwrap();
    catalog.save_model("same", "first").unwrap();
    b.save_model("same", "winner").unwrap();
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert_eq!(
        catalog.load_model("same").unwrap().as_deref(),
        Some("winner")
    );
}

#[test]
fn rejected_native_legacy_analyzer_write_keeps_the_prior_binding_in_one_atomic_batch() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 256 * 1024,
        })
        .unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog
        .replace_table_field_analyzer_binding("docs", "text", "index", "original", "binding")
        .unwrap();
    for explicit in [true, false] {
        if explicit {
            connection.begin_transaction().unwrap();
        }
        assert!(catalog
            .save_table_field_analyzer("docs", "text", "search", &"x".repeat(1024 * 1024))
            .is_err());
        assert_eq!(
            catalog.load_table_field_analyzer_bindings().unwrap(),
            vec![("docs".into(), "text".into(), "binding".into())]
        );
        assert_eq!(catalog.load_table_field_analyzers().unwrap().len(), 1);
        assert_eq!(connection.in_transaction(), explicit);
        if explicit {
            connection.commit_transaction().unwrap();
        }
    }
}

#[test]
fn native_catalog_failure_rolls_back_catalog_and_document_effects_before_retry() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let connection = open(mode, &directory.path().join("failure.db"));
        let catalog = Catalog::open(connection.clone()).unwrap();
        bind(&connection);
        let other = connection.new_session();
        let observer = Catalog::open(other.clone()).unwrap();
        let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
        connection.begin_transaction().unwrap();
        documents
            .put(1, BTreeMap::from([("n".into(), Value::Int(1))]))
            .unwrap();
        write(&catalog, "value", "{\"n\":1}");
        other.with_physical(|sqlite| {
            sqlite.execute_batch("CREATE TRIGGER injected_catalog_failure BEFORE INSERT ON _table_field_analyzers BEGIN SELECT RAISE(ABORT, 'injected catalog failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(connection.commit_transaction().is_err());
        assert_value(&observer, "value", None);
        assert!(SQLiteDocumentStore::new(other.clone(), "docs")
            .get(1)
            .unwrap()
            .is_none());
        other
            .with_physical(|sqlite| {
                sqlite.execute_batch("DROP TRIGGER injected_catalog_failure")?;
                Ok(())
            })
            .unwrap();
        connection.commit_transaction().unwrap();
        assert_value(&observer, "value", Some("{\"n\":1}"));
        assert!(SQLiteDocumentStore::new(other.clone(), "docs")
            .get(1)
            .unwrap()
            .is_some());
    }
}
