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
    check_metadata_prefix_work(true);
}

#[test]
fn serialized_metadata_prefix_work_is_independent_of_unrelated_records() {
    check_metadata_prefix_work(false);
}

fn check_metadata_prefix_work(native: bool) {
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
    if native {
        connection
            .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
            .unwrap();
    }
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
        steps.store(0, Ordering::Relaxed);
        let mut fixed = Vec::new();
        catalog
            .visit_metadata_with_key_length(prefix, key.len(), &mut |key, value| {
                fixed.push((key.to_owned(), value.to_owned()));
                Ok(())
            })
            .unwrap();
        assert_eq!(fixed, found);
        assert!(steps.load(Ordering::Relaxed) < 4096);
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
fn fixed_length_metadata_visits_use_utf8_bytes_and_keep_typed_failures() {
    for native in [false, true] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        if native {
            connection
                .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
                .unwrap();
        }
        let prefix = "scope:%_\0日本語:";
        for suffix in ["a", "aa", "bb", "long"] {
            catalog
                .set_metadata(&format!("{prefix}{suffix}"), suffix)
                .unwrap();
        }
        let mut values = Vec::new();
        CatalogFacade::visit_metadata_with_key_length(
            &catalog,
            prefix,
            prefix.len() + 2,
            &mut |_, value| {
                values.push(value.to_owned());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(values, ["aa", "bb"]);
        CatalogFacade::visit_metadata_with_key_length(
            &catalog,
            prefix,
            prefix.len() - 1,
            &mut |_, _| unreachable!(),
        )
        .unwrap();
        let mut calls = 0;
        let error = CatalogFacade::visit_metadata_with_key_length(
            &catalog,
            prefix,
            prefix.len() + 2,
            &mut |_, _| {
                calls += 1;
                Err(uqa_core::QueryCancelled::USER_REQUEST.into())
            },
        );
        assert!(matches!(error, Err(StorageBackendError::Cancelled(_))));
        assert_eq!(calls, 1);
    }
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

#[test]
fn borrowed_metadata_visits_keep_only_record_workspace() {
    for native in [false, true] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        let payload = "x".repeat(16 * 1024);
        connection.begin_transaction().unwrap();
        for index in 0..128 {
            catalog
                .set_metadata(&format!("payload:%_\0日本語:{index:04}"), &payload)
                .unwrap();
        }
        connection.commit_transaction().unwrap();
        if native {
            connection
                .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
                .unwrap();
        }
        let mut count = 0;
        let allocation = allocation_counter::measure(|| {
            CatalogFacade::visit_metadata_with_prefix(
                &catalog,
                "payload:%_\0日本語:",
                &mut |_, value| {
                    assert_eq!(value, payload);
                    count += 1;
                    Ok(())
                },
            )
            .unwrap();
        });
        assert_eq!(count, 128);
        // Two MiB of selected values must not become a retained payload collection. The bound includes native cursor, record decoding and SQLite statement workspace.
        assert!(
            allocation.bytes_max < 512 * 1024,
            "{native}: {allocation:?}"
        );
    }
}

#[test]
fn borrowed_metadata_visits_stop_on_typed_errors_without_changing_records() {
    for native in [false, true] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        if native {
            connection
                .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
                .unwrap();
        }
        catalog.set_metadata("borrowed:a", "one").unwrap();
        catalog.set_metadata("borrowed:b", "two").unwrap();
        let mut calls = 0;
        let result =
            CatalogFacade::visit_metadata_with_prefix(&catalog, "borrowed:", &mut |_, _| {
                calls += 1;
                Err(uqa_core::QueryCancelled::USER_REQUEST.into())
            });
        assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
        assert_eq!(calls, 1);
        let result =
            CatalogFacade::visit_metadata_with_prefix(&catalog, "borrowed:", &mut |_, _| {
                Err(StorageBackendError::Other("visitor rejection".into()))
            });
        assert!(
            matches!(result, Err(StorageBackendError::Other(message)) if message == "visitor rejection")
        );
        let result =
            CatalogFacade::visit_metadata_with_prefix(&catalog, "borrowed:", &mut |_, _| {
                Err(serde_json::from_str::<bool>("invalid").unwrap_err().into())
            });
        assert!(matches!(result, Err(StorageBackendError::Serde(_))));
        assert_eq!(catalog.metadata_with_prefix("borrowed:").unwrap().len(), 2);
    }
}

#[test]
fn borrowed_metadata_visits_check_native_cancellation_after_the_last_row() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    catalog.set_metadata("borrowed:one", "value").unwrap();
    let cancellation = connection
        .native_snapshot()
        .unwrap()
        .unwrap()
        .control
        .cancellation()
        .clone();
    let result = CatalogFacade::visit_metadata_with_prefix(&catalog, "borrowed:", &mut |_, _| {
        cancellation.cancel();
        Ok(())
    });
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
    let result =
        CatalogFacade::visit_metadata_with_prefix(&catalog, "absent", &mut |_, _| unreachable!());
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
}
