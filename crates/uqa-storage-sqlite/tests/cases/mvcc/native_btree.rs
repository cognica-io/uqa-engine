//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native document and B-tree APIs share private visibility, savepoints and atomic durable publication.

use super::{open, MODES};
use std::{collections::BTreeMap, sync::mpsc, time::Duration};
use uqa_core::Value;
use uqa_storage::{
    mvcc::VersionedSessionOptions, DocumentStore, PersistentStorageBackend, ValueIndexKey,
};
use uqa_storage_sqlite::{
    Catalog, ManagedConnection, SQLiteBTreeIndexStore, SQLiteDocumentStore, SQLiteStorageBackend,
};

fn keys() -> [ValueIndexKey; 2] {
    [
        ValueIndexKey::Column("n".into()),
        ValueIndexKey::Index("n".into()),
    ]
}

fn fields(n: i64) -> BTreeMap<String, Value> {
    BTreeMap::from([("n".into(), Value::Int(n))])
}

fn values(n: i64) -> BTreeMap<ValueIndexKey, Value> {
    BTreeMap::from([
        (keys()[0].clone(), Value::Int(n)),
        (
            keys()[1].clone(),
            Value::Row(vec![Value::Int(n), Value::Str("expression".into())]),
        ),
    ])
}

fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}

fn initialize(connection: &ManagedConnection) {
    Catalog::open(connection.clone()).unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    let indexes = SQLiteBTreeIndexStore::new(connection.clone());
    for id in [1, 2] {
        documents.put(id, fields(id as i64)).unwrap();
    }
    for key in keys() {
        indexes
            .replace(
                "docs",
                &key,
                &[(1, values(1)[&key].clone()), (2, values(2)[&key].clone())],
            )
            .unwrap();
    }
    bind(connection);
}

#[test]
fn native_documents_and_btree_postings_commit_together_while_another_writer_stays_private() {
    for mode in MODES {
        for ending in ["commit", "rollback", "savepoint"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("indexes.db");
            let connection = open(mode, &path);
            initialize(&connection);
            let backend = SQLiteStorageBackend::new(connection.clone());
            let mut a = backend.document_store("docs");
            connection.begin_transaction().unwrap();
            a.put(1, fields(10)).unwrap();
            backend
                .apply_btree_index_write("docs", 1, Some(&values(10)))
                .unwrap();
            connection.savepoint("keep").unwrap();
            a.put(1, fields(11)).unwrap();
            backend
                .apply_btree_index_write("docs", 1, Some(&values(11)))
                .unwrap();
            let other_path = path.clone();
            let (sent, received) = mpsc::channel();
            let writer = std::thread::spawn(move || {
                let connection = open(mode, &other_path);
                bind(&connection);
                let backend = SQLiteStorageBackend::new(connection.clone());
                connection.begin_transaction().unwrap();
                backend.document_store("docs").put(2, fields(20)).unwrap();
                backend
                    .apply_btree_index_write("docs", 2, Some(&values(20)))
                    .unwrap();
                connection.commit_transaction().unwrap();
                sent.send(()).unwrap();
            });
            let completed = received.recv_timeout(Duration::from_secs(20));
            if completed.is_err() {
                connection.rollback_transaction().unwrap();
                writer.join().unwrap();
                panic!("native indexed writer did not finish: {mode:?} {completed:?}");
            }
            writer.join().unwrap();
            assert!(connection.in_transaction());
            for key in keys() {
                assert_eq!(
                    backend.load_btree_index("docs", &key).unwrap(),
                    Some(vec![
                        (1, values(11)[&key].clone()),
                        (2, values(2)[&key].clone())
                    ])
                );
            }
            let expected = match ending {
                "commit" => {
                    connection.commit_transaction().unwrap();
                    11
                }
                "rollback" => {
                    connection.rollback_transaction().unwrap();
                    1
                }
                _ => {
                    connection.rollback_to_savepoint("keep").unwrap();
                    connection.commit_transaction().unwrap();
                    10
                }
            };
            drop(a);
            drop(backend);
            drop(connection);
            let reopened = open(mode, &path);
            bind(&reopened);
            let backend = SQLiteStorageBackend::new(reopened);
            assert_eq!(
                backend.document_store("docs").get(1).unwrap(),
                Some(fields(expected))
            );
            assert_eq!(
                backend.document_store("docs").get(2).unwrap(),
                Some(fields(20))
            );
            for key in keys() {
                assert_eq!(
                    backend.load_btree_index("docs", &key).unwrap(),
                    Some(vec![
                        (1, values(expected)[&key].clone()),
                        (2, values(20)[&key].clone())
                    ])
                );
            }
        }
    }
}

#[test]
fn native_btree_replacement_repair_delete_clear_and_namespaces_follow_one_snapshot() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    initialize(&connection);
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    let indexes = SQLiteBTreeIndexStore::new(connection.clone());
    assert_eq!(indexes.fields("docs").unwrap(), keys());
    let field = &keys()[0];
    let named = &keys()[1];
    documents.put(3, fields(3)).unwrap();
    indexes
        .repair("docs", field, &[1], &[(3, Value::Bytes(vec![0, 255]))])
        .unwrap();
    assert_eq!(
        indexes.load("docs", field).unwrap(),
        Some(vec![(2, Value::Int(2)), (3, Value::Bytes(vec![0, 255]))])
    );
    assert_eq!(indexes.load("docs", named).unwrap().unwrap().len(), 2);
    let other = connection.new_session();
    other.begin_deferred_transaction().unwrap();
    let old = SQLiteBTreeIndexStore::new(other.clone());
    indexes.apply_write("docs", 2, None).unwrap();
    assert_eq!(old.load("docs", field).unwrap().unwrap().len(), 2);
    assert_eq!(
        indexes.load("docs", field).unwrap(),
        Some(vec![(3, Value::Bytes(vec![0, 255]))])
    );
    indexes
        .apply_write(
            "docs",
            3,
            Some(&BTreeMap::from([(
                ValueIndexKey::Column("unbuilt".into()),
                Value::Int(4),
            )])),
        )
        .unwrap();
    assert!(indexes.load("docs", &"unbuilt".into()).unwrap().is_none());
    indexes
        .replace_many(
            "docs",
            &[
                (field, &[(1, Value::Null), (2, Value::Float(-0.0))]),
                (named, &[(3, Value::Row(vec![Value::Int(3)]))]),
            ],
        )
        .unwrap();
    let got = indexes.load("docs", field).unwrap().unwrap();
    assert_eq!(got[0], (1, Value::Null));
    let Value::Float(zero) = got[1].1 else {
        panic!("lost floating point storage class")
    };
    assert_eq!(zero.to_bits(), (-0.0_f64).to_bits());
    documents.delete(3).unwrap();
    assert_eq!(indexes.load("docs", named).unwrap(), Some(vec![]));
    indexes.clear_table("docs").unwrap();
    assert_eq!(indexes.fields("docs").unwrap(), keys());
    assert_eq!(indexes.load("docs", field).unwrap(), Some(vec![]));
    indexes.drop_index("docs", named).unwrap();
    assert_eq!(indexes.load("docs", named).unwrap(), None);
    assert_eq!(old.load("docs", named).unwrap().unwrap().len(), 2);
    other.rollback_transaction().unwrap();
    assert_eq!(old.fields("docs").unwrap(), vec![field.clone()]);
}

#[test]
fn native_btree_validation_errors_preserve_every_preexisting_private_posting() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    initialize(&connection);
    let indexes = SQLiteBTreeIndexStore::new(connection.clone());
    let field = &keys()[0];
    connection.begin_transaction().unwrap();
    indexes.apply_write("docs", 1, Some(&values(5))).unwrap();
    let expected = indexes.load("docs", field).unwrap();
    for invalid in [
        vec![(1, Value::Int(8)), (u64::MAX, Value::Null)],
        vec![(1, Value::Int(8)), (1, Value::Null)],
        vec![(1, Value::Int(8)), (77, Value::Null)],
    ] {
        assert!(indexes.replace("docs", field, &invalid).is_err());
        assert_eq!(indexes.load("docs", field).unwrap(), expected);
    }
    assert!(indexes
        .repair("docs", field, &[1], &[(77, Value::Null)])
        .is_err());
    assert_eq!(indexes.load("docs", field).unwrap(), expected);
    assert!(indexes.apply_write("docs", 77, Some(&values(10))).is_err());
    assert_eq!(indexes.load("docs", field).unwrap(), expected);
    connection.commit_transaction().unwrap();
    assert_eq!(indexes.load("docs", field).unwrap(), expected);
    assert!(indexes
        .replace("empty", &"new".into(), &[(77, Value::Null)])
        .is_err());
    assert!(!connection.in_transaction());
    assert_eq!(indexes.fields("empty").unwrap(), vec![]);
    indexes.replace("empty", &"new".into(), &[]).unwrap();
    assert_eq!(indexes.load("empty", &"new".into()).unwrap(), Some(vec![]));
}

#[test]
fn native_btree_repair_markers_are_ordered_and_rollback_with_their_session() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .with(|sqlite| {
            sqlite.execute_batch(
                "INSERT INTO _btree_index_repairs VALUES ('z','a'),('a',x'6e'),('a','n')",
            )?;
            Ok(())
        })
        .unwrap();
    bind(&connection);
    let indexes = SQLiteBTreeIndexStore::new(connection.clone());
    let expected = vec![
        ("a".into(), keys()[0].clone()),
        ("a".into(), keys()[1].clone()),
        ("z".into(), "a".into()),
    ];
    assert_eq!(indexes.repairs().unwrap(), expected);
    connection.begin_transaction().unwrap();
    indexes.clear_repair("a", &keys()[0]).unwrap();
    assert_eq!(indexes.repairs().unwrap().len(), 2);
    connection.rollback_transaction().unwrap();
    assert_eq!(indexes.repairs().unwrap(), expected);
    indexes.clear_repair("a", &keys()[1]).unwrap();
    assert_eq!(
        indexes.repairs().unwrap(),
        vec![("a".into(), keys()[0].clone()), ("z".into(), "a".into())]
    );
}

#[test]
fn concurrent_native_index_drop_and_document_delete_reject_stale_posting_publication() {
    for delete_document in [false, true] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        initialize(&connection);
        let indexes = SQLiteBTreeIndexStore::new(connection.clone());
        let other = connection.new_session();
        let b = SQLiteBTreeIndexStore::new(other.clone());
        connection.begin_transaction().unwrap();
        indexes.apply_write("docs", 1, Some(&values(10))).unwrap();
        if delete_document {
            SQLiteDocumentStore::new(other.clone(), "docs")
                .delete(1)
                .unwrap();
        } else {
            b.drop_index("docs", &keys()[0]).unwrap();
        }
        assert!(connection.commit_transaction().is_err());
        connection.rollback_transaction().unwrap();
        if delete_document {
            assert_eq!(
                b.load("docs", &keys()[0]).unwrap(),
                Some(vec![(2, Value::Int(2))])
            );
        } else {
            assert_eq!(b.load("docs", &keys()[0]).unwrap(), None);
        }
    }
}

#[test]
fn native_index_failure_rolls_back_document_and_index_materialization_before_exact_retry() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let connection = open(mode, &directory.path().join("fault.db"));
        initialize(&connection);
        let indexes = SQLiteBTreeIndexStore::new(connection.clone());
        let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
        let other = connection.new_session();
        let observer = SQLiteBTreeIndexStore::new(other.clone());
        let observed_documents = SQLiteDocumentStore::new(other.clone(), "docs");
        connection.begin_transaction().unwrap();
        documents.put(1, fields(10)).unwrap();
        indexes.apply_write("docs", 1, Some(&values(10))).unwrap();
        indexes
            .replace("docs", &"new".into(), &[(1, Value::Int(10))])
            .unwrap();
        other.with_physical(|sqlite| {
            sqlite.execute_batch("CREATE TRIGGER injected_native_btree_failure BEFORE INSERT ON _btree_index_entries WHEN NEW.field='new' BEGIN SELECT RAISE(ABORT, 'injected B-tree failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(connection.commit_transaction().is_err());
        assert_eq!(observed_documents.get(1).unwrap(), Some(fields(1)));
        assert_eq!(observer.fields("docs").unwrap(), keys());
        for key in keys() {
            assert_eq!(
                observer.load("docs", &key).unwrap().unwrap()[0].1,
                values(1)[&key]
            );
        }
        other
            .with_physical(|sqlite| {
                sqlite.execute_batch("DROP TRIGGER injected_native_btree_failure")?;
                Ok(())
            })
            .unwrap();
        connection.commit_transaction().unwrap();
        assert_eq!(observed_documents.get(1).unwrap(), Some(fields(10)));
        assert_eq!(
            observer.load("docs", &"new".into()).unwrap(),
            Some(vec![(1, Value::Int(10))])
        );
        for key in keys() {
            assert_eq!(
                observer.load("docs", &key).unwrap().unwrap()[0].1,
                values(10)[&key]
            );
        }
    }
}
