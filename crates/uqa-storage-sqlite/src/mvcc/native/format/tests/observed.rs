//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! What an open tells its observer of an upgrade, and what an inspection reads of it beforehand.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::equality::write_the_keys_of_the_format_before;
use super::*;
use crate::upgrade::{StorageUpgradeEvent as Event, StorageUpgradeObserver};
use crate::{
    inspect_storage_upgrade, SQLiteBTreeIndexStore, SQLiteCompressionOptions, SQLiteDocumentStore,
    SQLiteError,
};
use uqa_core::Value;
use uqa_storage::DocumentStore;

const KEY: &str = "storage upgrade test";
const ENTRIES: u64 = 40;

#[derive(Default)]
struct Told(Mutex<Vec<Event>>);

impl StorageUpgradeObserver for Told {
    fn observe(&self, event: &Event) {
        self.0.lock().unwrap().push(event.clone());
    }
}

impl Told {
    /// What was told, without the reports of a step that is at work: how many of those there are depends on the machine.
    fn events(&self) -> Vec<Event> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|event| !matches!(event, Event::StepAdvanced { .. }))
            .cloned()
            .collect()
    }
}

fn connection(path: &Path, mode: usize) -> ManagedConnection {
    match mode {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, KEY),
        2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
        _ => ManagedConnection::open_compressed_encrypted(
            path,
            KEY,
            SQLiteCompressionOptions::default(),
        ),
    }
    .unwrap()
}

fn key_of(mode: usize) -> Option<&'static str> {
    (mode % 2 == 1).then_some(KEY)
}

/// A database with `ENTRIES` index entries, put back into the mapping before the ordered keys.
fn in_the_mapping_before(connection: &ManagedConnection) -> StorageReadControl {
    Catalog::open(connection.clone()).unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "items");
    let entries: Vec<_> = (1..=ENTRIES)
        .map(|id| (id, Value::Int(i64::try_from(id).unwrap() * 10 - 200)))
        .collect();
    for (id, value) in &entries {
        documents
            .put(*id, [("key".into(), value.clone())].into())
            .unwrap();
    }
    SQLiteBTreeIndexStore::new(connection.clone())
        .replace("items", &"key".into(), &entries)
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 22);
    let namespace = SQLiteRecordStore::for_native(connection, &control)
        .unwrap()
        .native_namespace()
        .unwrap();
    write_the_keys_of_the_format_before(connection, namespace);
    control
}

fn assert_the_upgrade_of_the_keys_was_told(events: &[Event]) {
    assert!(
        matches!(
            events,
            [
                Event::Started {
                    from: 17,
                    to: CURRENT_VERSION
                },
                Event::StepStarted {
                    step: StorageUpgradeStep::LookupKeys,
                    rows: Some(ENTRIES)
                },
                Event::StepFinished {
                    step: StorageUpgradeStep::LookupKeys,
                    ..
                },
                Event::Committing,
                Event::Finished { .. },
            ]
        ),
        "{events:?}"
    );
}

#[test]
fn the_steps_that_write_rows_again_follow_from_the_mapping_a_database_is_stored_in() {
    use StorageUpgradeStep::{GraphLookups, LookupKeys, SequenceValues};
    assert_eq!(
        rewriting_steps(1),
        [GraphLookups, SequenceValues, LookupKeys]
    );
    assert_eq!(
        rewriting_steps(2),
        [GraphLookups, SequenceValues, LookupKeys]
    );
    for version in 3..SEQUENCE_VALUES_VERSION {
        assert_eq!(rewriting_steps(version), [SequenceValues, LookupKeys]);
    }
    for version in SEQUENCE_VALUES_VERSION..ORDERED_KEYS_VERSION {
        assert_eq!(rewriting_steps(version), [LookupKeys]);
    }
    assert_eq!(rewriting_steps(CURRENT_VERSION), []);
    assert_eq!(
        [GraphLookups, SequenceValues, LookupKeys].map(StorageUpgradeStep::name),
        ["graph_lookups", "sequence_values", "lookup_keys"]
    );
}

#[test]
fn an_initialization_tells_its_observer_the_upgrade_and_its_commit() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let control = in_the_mapping_before(&connection);
    let told = Arc::new(Told::default());
    connection.observe_storage_upgrade(told.clone());
    SQLiteRecordStore::for_native(&connection, &control).unwrap();
    assert_the_upgrade_of_the_keys_was_told(&told.events());

    // A database that is current tells nothing.
    let again = Arc::new(Told::default());
    connection.observe_storage_upgrade(again.clone());
    SQLiteRecordStore::for_native(&connection, &control).unwrap();
    assert_eq!(again.events(), []);
}

#[test]
fn an_upgrade_from_a_mapping_without_populations_tells_its_step_without_rows() {
    // The mapping before 16 keeps no population of its indexes: nothing says how many keys there are without reading them.
    let connection = ManagedConnection::open_in_memory().unwrap();
    let control = in_the_mapping_before(&connection);
    let namespace = SQLiteRecordStore::for_native(&connection, &control)
        .unwrap()
        .native_namespace()
        .unwrap();
    crate::mvcc::native::tests::materialization::with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        for (name, _) in crate::btree_index::equality::triggers() {
            transaction.execute_batch(&format!("DROP TRIGGER {name}"))?;
        }
        transaction.execute_batch("DROP TABLE _btree_equality_keys")?;
        for (name, _) in crate::btree_index::coverage::triggers() {
            transaction.execute_batch(&format!("DROP TRIGGER {name}"))?;
        }
        for (name, _) in crate::btree_index::coverage::TABLES {
            transaction.execute_batch(&format!("DROP TABLE {name}"))?;
        }
        transaction.execute_batch("CREATE INDEX _btree_index_value_idx ON _btree_index_entries(table_name, field, value_json, doc_id); DROP TABLE _uqa_mvcc_native_format")?;
        transaction.execute_batch(FORMAT_FIFTEEN)?;
        transaction.execute(
            "INSERT INTO _uqa_mvcc_native_format VALUES(1,15,49,?1)",
            [namespace.as_bytes().as_slice()],
        )?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger(TABLES[0].0, action).1)?;
        }
        transaction.commit()?;
        validate_format(sqlite, 15)?;
        let inspection = inspect(sqlite)?;
        assert_eq!(inspection.stored_mapping(), Some(15));
        assert_eq!(inspection.steps(), [(StorageUpgradeStep::LookupKeys, None)]);
        Ok(())
    });
    let told = Arc::new(Told::default());
    connection.observe_storage_upgrade(told.clone());
    SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let events = told.events();
    assert!(
        matches!(
            events.as_slice(),
            [
                Event::Started {
                    from: 15,
                    to: CURRENT_VERSION
                },
                Event::StepStarted {
                    step: StorageUpgradeStep::LookupKeys,
                    rows: None
                },
                Event::StepFinished { .. },
                Event::Committing,
                Event::Finished { .. },
            ]
        ),
        "{events:?}"
    );
}

#[test]
fn an_inspection_reads_what_an_open_will_do_and_changes_no_file_in_any_file_mode() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let path = directory.path().join(format!("inspected-{mode}.db"));
        let key = key_of(mode);
        // A file that does not exist is created by an open and upgraded by none.
        let missing = inspect_storage_upgrade(&path, key).unwrap();
        assert_eq!(missing.stored_mapping(), None);
        assert_eq!(missing.current_mapping(), CURRENT_VERSION);
        assert!(!missing.upgrade_due());
        assert!(!path.exists());

        {
            let connection = connection(&path, mode);
            in_the_mapping_before(&connection);
        }
        let stored = std::fs::read(&path).unwrap();
        let inspection = inspect_storage_upgrade(&path, key).unwrap();
        assert_eq!(inspection.stored_mapping(), Some(17), "mode {mode}");
        assert_eq!(inspection.current_mapping(), CURRENT_VERSION);
        assert!(inspection.upgrade_due());
        assert_eq!(
            inspection.steps(),
            [(StorageUpgradeStep::LookupKeys, Some(ENTRIES))]
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            stored,
            "the inspection changed the file of mode {mode}"
        );
        // A key that does not fit the file is refused as an open refuses it.
        match key {
            Some(_) => assert!(matches!(
                inspect_storage_upgrade(&path, None),
                Err(SQLiteError::EncryptionKeyRequired)
            )),
            None => assert!(matches!(
                inspect_storage_upgrade(&path, Some(KEY)),
                Err(SQLiteError::NotEncrypted)
            )),
        }
        assert!(matches!(
            inspect_storage_upgrade(&path, Some("")),
            Err(SQLiteError::EmptyEncryptionKey)
        ));

        // The open of an engine binds the mapping at its initial restore: it performs what the inspection read, and tells it.
        let connection = connection(&path, mode);
        let told = Arc::new(Told::default());
        connection.observe_storage_upgrade(told.clone());
        connection.begin_native_initial_restore().unwrap();
        assert_the_upgrade_of_the_keys_was_told(&told.events());
        connection.rollback_transaction().unwrap();
        drop(connection);

        let current = inspect_storage_upgrade(&path, key).unwrap();
        assert_eq!(
            current.stored_mapping(),
            Some(CURRENT_VERSION),
            "mode {mode}"
        );
        assert!(!current.upgrade_due());
        assert!(current.steps().is_empty());

        // The next open finds nothing to upgrade and tells nothing.
        let connection = connection_again(&path, mode);
        let told = Arc::new(Told::default());
        connection.observe_storage_upgrade(told.clone());
        connection.begin_native_initial_restore().unwrap();
        assert_eq!(told.events(), []);
        connection.rollback_transaction().unwrap();
    }
}

fn connection_again(path: &Path, mode: usize) -> ManagedConnection {
    ManagedConnection::open_auto(path, key_of(mode)).unwrap()
}
