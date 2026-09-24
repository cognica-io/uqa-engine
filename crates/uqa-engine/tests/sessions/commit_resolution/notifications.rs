//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Real file registries retain the original publication while main commit resolution is pending.

use super::*;

#[test]
fn file_notification_publication_retains_its_original_read_only_commit_attempt() {
    for resolve_with_rollback in [false, true] {
        let (directory, fixtures) = fixtures();
        for (index, persistence) in fixtures.into_iter().enumerate() {
            let name = match index {
                0 => "plain.db",
                4 => "receipt.redb",
                _ => continue,
            };
            let store: Arc<dyn KeyValueStore> = Arc::new(VersionedKeyValueStore::new(
                persistence.clone(),
                Some(uqa_storage::PersistentStorageIdentity::File(
                    directory.path().join(name),
                )),
                VersionedSessionOptions::default(),
            ));
            let root = Engine::from_persistent_backends(
                Arc::new(KeyValueCatalog::new(store.clone())),
                Arc::new(KeyValueStorageBackend::new(store)),
            )
            .unwrap();
            let listener = root.new_session().unwrap();
            listener.sql("LISTEN commit_events", &[]).unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let callback_calls = calls.clone();
            root.register_scalar_function_with_options(
                "notification_probe",
                SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                move |_: &[Value]| {
                    callback_calls.fetch_add(1, Ordering::AcqRel);
                    Ok(Value::Str("original payload".into()))
                },
            )
            .unwrap();
            root.sql(
                "BEGIN READ ONLY; SELECT pg_notify('commit_events', notification_probe())",
                &[],
            )
            .unwrap();
            persistence
                .fault
                .store(LOSE_COMMITTED_REPLY, Ordering::Release);
            assert_unknown(&root.commit().unwrap_err());
            let identity = root.pending_commit().unwrap();
            assert_unknown(&root.commit().unwrap_err());
            assert_eq!(root.pending_commit(), Some(identity));
            assert_eq!(calls.load(Ordering::Acquire), 1);
            persistence.fault.store(HEALTHY, Ordering::Release);
            if resolve_with_rollback {
                assert_eq!(root.rollback().unwrap_err().sqlstate(), Some("25000"));
            } else {
                root.commit().unwrap();
            }
            assert!(root.pending_commit().is_none());
            listener.poll_sql_notifications().unwrap();
            let delivered = listener.take_sql_notifications();
            assert_eq!(delivered.len(), 1);
            assert_eq!(delivered[0].process_id, root.backend_process_id());
            assert_eq!(delivered[0].payload, "original payload");
            listener.poll_sql_notifications().unwrap();
            assert!(listener.take_sql_notifications().is_empty());
            assert_eq!(calls.load(Ordering::Acquire), 1);
        }
    }
}
