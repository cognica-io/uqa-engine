//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence catalog and value operations share the selected session's atomic publication boundary.

use super::{native_tables::schema, open, MODES};
use std::{collections::BTreeMap, sync::mpsc, time::Duration};
use uqa_core::Value;
use uqa_storage::{
    mvcc::VersionedSessionOptions, DocumentStore, RelationIdentity, SequenceAclEntry,
    SequenceOptions, SequenceOwner, SequenceOwnerDependency, SequencePrivileges,
    SequenceReservationResult, SequenceRow, SequenceValueReservation,
};
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteDocumentStore};

#[path = "native_sequences/lifecycle.rs"]
mod lifecycle;
#[path = "native_sequences/values.rs"]
mod values;

fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}

fn memory(native: bool) -> (ManagedConnection, Catalog) {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    if native {
        bind(&connection);
    }
    (connection, catalog)
}

fn sequence(name: &str, id: u8) -> SequenceRow {
    SequenceRow {
        relation: RelationIdentity::new("public", name),
        role_owner: "owner".into(),
        acl: None,
        object_id: [id; 16],
        definition_generation: [id + 64; 16],
        start: 1,
        increment: 1,
        current: 1,
        called: false,
        log_count: 0,
        persistence: "p".into(),
        owner: None,
        options: SequenceOptions {
            min_value: Some(1),
            max_value: Some(i64::MAX),
            cache_size: 3,
            ..SequenceOptions::default()
        },
    }
}

fn reserve(catalog: &Catalog, row: &SequenceRow) -> SequenceValueReservation {
    let SequenceReservationResult::Reserved(result) = catalog
        .reserve_sequence_values(
            &row.relation.qualified_name(),
            row.object_id,
            row.definition_generation,
        )
        .unwrap()
    else {
        panic!("expected sequence reservation")
    };
    result
}

fn fields(value: i64) -> BTreeMap<String, Value> {
    BTreeMap::from([("n".into(), Value::Int(value))])
}

#[test]
fn independent_native_sequence_writers_publish_before_the_other_transaction_ends() {
    for mode in MODES {
        for ending in ["commit", "rollback", "savepoint"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("sequences.db");
            let connection = open(mode, &path);
            let catalog = Catalog::open(connection.clone()).unwrap();
            catalog.save_table(&schema("docs", 30, 30)).unwrap();
            bind(&connection);
            let a = sequence("a", 1);
            connection.begin_transaction().unwrap();
            assert!(catalog.create_sequence_row(&a).unwrap());
            assert_eq!(reserve(&catalog, &a).last_value, 3);
            let mut documents = SQLiteDocumentStore::new(connection.clone(), "public.docs");
            documents.put(1, fields(3)).unwrap();
            connection.savepoint("keep").unwrap();
            catalog
                .set_sequence_value("a", a.object_id, a.definition_generation, 40, false, 0)
                .unwrap();
            assert_eq!(reserve(&catalog, &a).last_value, 42);
            documents.put(1, fields(42)).unwrap();
            let other_path = path.clone();
            let (sent, received) = mpsc::channel();
            let writer = std::thread::spawn(move || {
                let connection = open(mode, &other_path);
                bind(&connection);
                let catalog = Catalog::open(connection.clone()).unwrap();
                connection.begin_transaction().unwrap();
                let mut b = sequence("b", 2);
                b.start = 101;
                b.current = 101;
                assert!(catalog.create_sequence_row(&b).unwrap());
                assert_eq!(reserve(&catalog, &b).last_value, 103);
                SQLiteDocumentStore::new(connection.clone(), "public.docs")
                    .put(2, fields(103))
                    .unwrap();
                connection.commit_transaction().unwrap();
                sent.send(()).unwrap();
            });
            let completed = received.recv_timeout(Duration::from_secs(20));
            if completed.is_err() {
                connection.rollback_transaction().unwrap();
                writer.join().unwrap();
                panic!("sequence writer did not finish: {mode:?} {completed:?}");
            }
            writer.join().unwrap();
            assert!(connection.in_transaction());
            assert_eq!(catalog.load_sequence_rows().unwrap().len(), 1);
            assert_eq!(documents.get(2).unwrap(), None);
            let expected = match ending {
                "commit" => {
                    connection.commit_transaction().unwrap();
                    Some(42)
                }
                "rollback" => {
                    connection.rollback_transaction().unwrap();
                    None
                }
                _ => {
                    connection.rollback_to_savepoint("keep").unwrap();
                    connection.commit_transaction().unwrap();
                    Some(3)
                }
            };
            drop(documents);
            drop(catalog);
            drop(connection);
            let reopened = open(mode, &path);
            bind(&reopened);
            let catalog = Catalog::open(reopened.clone()).unwrap();
            let rows = catalog.load_sequence_rows().unwrap();
            assert_eq!(
                rows.iter()
                    .find(|row| row.relation.name == "a")
                    .map(|row| row.current),
                expected
            );
            assert_eq!(
                rows.iter()
                    .find(|row| row.relation.name == "b")
                    .unwrap()
                    .current,
                103
            );
            let documents = SQLiteDocumentStore::new(reopened, "public.docs");
            assert_eq!(documents.get(1).unwrap(), expected.map(fields));
            assert_eq!(documents.get(2).unwrap(), Some(fields(103)));
        }
    }
}

#[test]
fn native_sequence_session_commits_values_while_another_session_has_private_documents() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("autonomous.db");
        let connection = open(mode, &path);
        let catalog = Catalog::open(connection.clone()).unwrap();
        catalog.save_table(&schema("docs", 30, 30)).unwrap();
        let row = sequence("s", 1);
        catalog.create_sequence_row(&row).unwrap();
        bind(&connection);
        connection.begin_transaction().unwrap();
        let mut documents = SQLiteDocumentStore::new(connection.clone(), "public.docs");
        documents.put(1, fields(1)).unwrap();
        let other = connection.new_session();
        let (sent, received) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let catalog = Catalog::open(other).unwrap();
            assert_eq!(reserve(&catalog, &row).first_value, 1);
            assert_eq!(
                catalog
                    .set_sequence_value(
                        "s",
                        row.object_id,
                        row.definition_generation,
                        200,
                        false,
                        0
                    )
                    .unwrap(),
                uqa_storage::SequenceSetValueResult::Set(200)
            );
            assert_eq!(reserve(&catalog, &row).first_value, 200);
            sent.send(()).unwrap();
        });
        let completed = received.recv_timeout(Duration::from_secs(20));
        if completed.is_err() {
            connection.rollback_transaction().unwrap();
            writer.join().unwrap();
            panic!("sequence allocation did not finish: {mode:?} {completed:?}");
        }
        writer.join().unwrap();
        assert_eq!(catalog.load_sequence_rows().unwrap(), [sequence("s", 1)]);
        connection.rollback_transaction().unwrap();
        assert_eq!(documents.get(1).unwrap(), None);
        assert_eq!(catalog.load_sequence_rows().unwrap()[0].current, 202);
        drop(documents);
        drop(catalog);
        drop(connection);
        let reopened = open(mode, &path);
        bind(&reopened);
        let catalog = Catalog::open(reopened).unwrap();
        assert_eq!(reserve(&catalog, &sequence("s", 1)).first_value, 203);
    }
}

#[test]
fn native_sequence_publication_failure_retries_the_evaluated_reservation_exactly_once() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let connection = open(mode, &directory.path().join("failure.db"));
        let catalog = Catalog::open(connection.clone()).unwrap();
        bind(&connection);
        let original = sequence("old", 1);
        catalog.create_sequence_row(&original).unwrap();
        let other = connection.new_session();
        let observer = Catalog::open(other.clone()).unwrap();
        connection.begin_transaction().unwrap();
        assert!(catalog.rename_sequence_row("old", "renamed").unwrap());
        let mut renamed = original.clone();
        renamed.relation.name = "renamed".into();
        renamed.definition_generation = [9; 16];
        assert!(catalog.replace_sequence_row(&renamed).unwrap());
        assert_eq!(reserve(&catalog, &renamed).last_value, 3);
        catalog.create_sequence_row(&sequence("old", 2)).unwrap();
        catalog.save_model("paired", "model").unwrap();
        let expected = catalog.load_sequence_rows().unwrap();
        other.with_physical(|sql| {
            sql.execute_batch("CREATE TRIGGER injected_sequence_failure BEFORE INSERT ON _sequences BEGIN SELECT RAISE(ABORT, 'sequence failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(connection.commit_transaction().is_err());
        assert_eq!(observer.load_sequence_rows().unwrap(), [original]);
        assert_eq!(observer.load_model("paired").unwrap(), None);
        other
            .with_physical(|sql| {
                sql.execute_batch("DROP TRIGGER injected_sequence_failure")?;
                Ok(())
            })
            .unwrap();
        connection.commit_transaction().unwrap();
        assert_eq!(observer.load_sequence_rows().unwrap(), expected);
        assert_eq!(
            observer.load_model("paired").unwrap().as_deref(),
            Some("model")
        );
        assert_eq!(reserve(&observer, &renamed).first_value, 4);
    }
}
