//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{Arc, Mutex};

use super::Engine;
use crate::{StorageUpgradeEvent, StorageUpgradeObserver};

#[derive(Default)]
struct Told(Mutex<Vec<StorageUpgradeEvent>>);

impl StorageUpgradeObserver for Told {
    fn observe(&self, event: &StorageUpgradeEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

#[rstest::rstest]
#[case::plain(None)]
#[case::encrypted(Some("storage upgrade test"))]
fn a_database_this_engine_wrote_is_inspected_as_current_and_opens_without_an_upgrade(
    #[case] key: Option<&str>,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("current.db");
    // A file that does not exist is created by an open and upgraded by none.
    let missing = Engine::inspect_storage_upgrade(&path, key).unwrap();
    assert_eq!(missing.stored_mapping(), None);
    assert!(!missing.upgrade_due());

    {
        let engine = Engine::open_auto(&path, key).unwrap();
        engine
            .sql(
                "CREATE TABLE items(id integer PRIMARY KEY, label text)",
                &[],
            )
            .unwrap();
        engine
            .sql("CREATE INDEX items_label ON items(label)", &[])
            .unwrap();
        engine
            .sql("INSERT INTO items VALUES (1, 'one'), (2, 'two')", &[])
            .unwrap();
        engine.close().unwrap();
    }

    let inspection = Engine::inspect_storage_upgrade(&path, key).unwrap();
    assert_eq!(
        inspection.stored_mapping(),
        Some(inspection.current_mapping())
    );
    assert!(!inspection.upgrade_due());
    assert!(inspection.steps().is_empty());

    // The open of a database that is current tells its observer nothing, and the engine it returns reads what was written.
    let told = Arc::new(Told::default());
    let engine = Engine::open_auto_with_upgrade_observer(&path, key, told.clone()).unwrap();
    assert_eq!(told.0.lock().unwrap().as_slice(), []);
    let rows = engine
        .sql(
            "SELECT label FROM items WHERE label >= 'one' ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
}
