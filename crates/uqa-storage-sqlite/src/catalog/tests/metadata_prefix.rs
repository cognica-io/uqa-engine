//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata prefix selection must not visit unrelated catalog payloads.

use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[test]
fn native_metadata_prefix_work_is_independent_of_unrelated_records() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection.begin_transaction().unwrap();
    for index in 0..1024 {
        catalog
            .set_metadata(&format!("unrelated:{index:04}"), "payload")
            .unwrap();
    }
    let key = "wanted:%_\0\u{65e5}:one";
    catalog.set_metadata(key, "selected").unwrap();
    connection.commit_transaction().unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    let steps = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&steps);
    connection
        .with_physical(|sqlite| {
            sqlite.progress_handler(
                100,
                Some(move || {
                    counter.fetch_add(100, Ordering::Relaxed);
                    false
                }),
            )?;
            Ok(())
        })
        .unwrap();
    for prefix in ["wanted:%_\0\u{65e5}", "absent:"] {
        steps.store(0, Ordering::Relaxed);
        let found = catalog.metadata_with_prefix(prefix).unwrap();
        assert_eq!(
            found,
            if prefix == "absent:" {
                Vec::new()
            } else {
                vec![(key.into(), "selected".into())]
            }
        );
        assert!(
            steps.load(Ordering::Relaxed) < 4096,
            "a metadata prefix must seek its range, not scan unrelated records"
        );
    }
    steps.store(0, Ordering::Relaxed);
    assert!(catalog.load_tables().unwrap().is_empty());
    assert!(
        steps.load(Ordering::Relaxed) < 4096,
        "relation ACL restoration must not scan unrelated metadata"
    );
    connection
        .with_physical(|sqlite| {
            sqlite.progress_handler(0, None::<fn() -> bool>)?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn metadata_prefix_preserves_private_changes_rollback_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("prefix.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    for (key, value) in [
        ("scope", "short"),
        ("scope:", "empty"),
        ("scope:a", "old"),
        ("scopez", "outside"),
    ] {
        catalog.set_metadata(key, value).unwrap();
    }
    connection.begin_transaction().unwrap();
    connection.savepoint("prefix").unwrap();
    catalog.delete_metadata("scope:").unwrap();
    catalog.set_metadata("scope:a", "changed").unwrap();
    catalog.set_metadata("scope:\0", "zero").unwrap();
    assert_eq!(
        catalog.metadata_with_prefix("scope:").unwrap(),
        vec![
            ("scope:\0".into(), "zero".into()),
            ("scope:a".into(), "changed".into()),
        ]
    );
    connection.rollback_to_savepoint("prefix").unwrap();
    let expected = vec![
        ("scope:".into(), "empty".into()),
        ("scope:a".into(), "old".into()),
    ];
    assert_eq!(catalog.metadata_with_prefix("scope:").unwrap(), expected);
    connection.commit_transaction().unwrap();
    drop((catalog, connection));
    let reopened = ManagedConnection::open(&path).unwrap();
    reopened
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    let catalog = Catalog::open(reopened).unwrap();
    assert_eq!(catalog.metadata_with_prefix("scope:").unwrap(), expected);
    let all = catalog.metadata_with_prefix("").unwrap();
    assert!(all.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert!(all.contains(&("scopez".into(), "outside".into())));
}
