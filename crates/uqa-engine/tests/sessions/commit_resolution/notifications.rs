//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Real file registries retain the original publication while main commit resolution is pending.

use super::*;

fn file_engines() -> (tempfile::TempDir, Vec<(Arc<FaultPersistence>, Engine)>) {
    let (directory, fixtures) = fixtures();
    let engines = fixtures
        .into_iter()
        .enumerate()
        .filter_map(|(index, persistence)| {
            let name = match index {
                0 => "plain.db",
                4 => "receipt.redb",
                _ => return None,
            };
            let store: Arc<dyn KeyValueStore> = Arc::new(VersionedKeyValueStore::new(
                persistence.clone(),
                Some(uqa_storage::PersistentStorageIdentity::File(
                    directory.path().join(name),
                )),
                VersionedSessionOptions::default(),
            ));
            let engine = Engine::from_persistent_backends(
                Arc::new(KeyValueCatalog::new(store.clone())),
                Arc::new(KeyValueStorageBackend::new(store)),
            )
            .unwrap();
            Some((persistence, engine))
        })
        .collect();
    (directory, engines)
}

#[test]
fn file_notification_publication_retains_its_original_read_only_commit_attempt() {
    for resolve_with_rollback in [false, true] {
        let (_directory, engines) = file_engines();
        for (persistence, root) in engines {
            let listener = root.new_session().unwrap();
            listener.sql("LISTEN commit_events", &[]).unwrap();
            let peer = root.new_session().unwrap();
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
            listener.poll_sql_notifications().unwrap();
            let delivered = listener.take_sql_notifications();
            assert_eq!(delivered.len(), 1);
            assert_eq!(delivered[0].process_id, root.backend_process_id());
            assert_eq!(delivered[0].payload, "original payload");
            peer.sql("NOTIFY commit_events, 'later payload'", &[])
                .unwrap();
            assert_unknown(&root.commit().unwrap_err());
            assert_eq!(root.pending_commit(), Some(identity));
            assert_eq!(calls.load(Ordering::Acquire), 1);
            listener.poll_sql_notifications().unwrap();
            let delivered = listener.take_sql_notifications();
            assert_eq!(delivered.len(), 1);
            assert_eq!(delivered[0].process_id, peer.backend_process_id());
            assert_eq!(delivered[0].payload, "later payload");
            persistence.fault.store(HEALTHY, Ordering::Release);
            if resolve_with_rollback {
                assert_eq!(root.rollback().unwrap_err().sqlstate(), Some("25000"));
            } else {
                root.commit().unwrap();
            }
            assert!(root.pending_commit().is_none());
            listener.poll_sql_notifications().unwrap();
            assert!(listener.take_sql_notifications().is_empty());
            assert_eq!(calls.load(Ordering::Acquire), 1);
        }
    }
}

#[test]
fn unresolved_uncommitted_notifications_allow_consumers_to_finish_transactions() {
    for resolve_with_rollback in [false, true] {
        let (_directory, engines) = file_engines();
        for (persistence, root) in engines {
            let listener = root.new_session().unwrap();
            listener.sql("LISTEN commit_events", &[]).unwrap();
            let peer = root.new_session().unwrap();
            root.sql("BEGIN READ ONLY; NOTIFY commit_events, 'reserved'", &[])
                .unwrap();
            persistence
                .fault
                .store(LOSE_UNCOMMITTED_REPLY, Ordering::Release);
            assert_unknown(&root.commit().unwrap_err());
            let identity = root.pending_commit().unwrap();
            listener.sql("BEGIN; SELECT 1; COMMIT", &[]).unwrap();
            listener.poll_sql_notifications().unwrap();
            assert!(listener.take_sql_notifications().is_empty());
            assert_unknown(&root.commit().unwrap_err());
            assert_eq!(root.pending_commit(), Some(identity));
            listener.sql("BEGIN; ROLLBACK", &[]).unwrap();

            persistence.fault.store(HEALTHY, Ordering::Release);
            if resolve_with_rollback {
                root.rollback().unwrap();
            } else {
                root.commit().unwrap();
            }
            assert!(root.pending_commit().is_none());
            listener.poll_sql_notifications().unwrap();
            let delivered = listener.take_sql_notifications();
            if resolve_with_rollback {
                assert!(delivered.is_empty());
            } else {
                assert_eq!(delivered.len(), 1);
                assert_eq!(delivered[0].payload, "reserved");
                assert_eq!(delivered[0].process_id, root.backend_process_id());
            }
            peer.sql("NOTIFY commit_events, 'after resolution'", &[])
                .unwrap();
            listener.poll_sql_notifications().unwrap();
            let delivered = listener.take_sql_notifications();
            assert_eq!(delivered.len(), 1);
            assert_eq!(delivered[0].payload, "after resolution");
            assert_eq!(delivered[0].process_id, peer.backend_process_id());
        }
    }
}
