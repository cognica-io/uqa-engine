//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::Ordering;

#[test]
fn internal_read_sessions_and_fixed_snapshots_do_not_register_maintenance_clients() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("internal-reads.db");
        let root = match provider {
            0 => Engine::open(&path).unwrap(),
            1 => Engine::from_persistent_provider(Arc::new(
                uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            2 => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            _ => unreachable!(),
        };
        root.sql("CREATE TABLE t(v integer); INSERT INTO t VALUES(1)", &[])
            .unwrap();
        root.release_automatic_statistics_client();
        root.session
            .statistics_worker
            .store(true, Ordering::Release);
        let internal = root.new_internal_read_session().unwrap();
        assert!(internal.session.statistics_worker.load(Ordering::Acquire));
        assert!(!internal.session.statistics_client.load(Ordering::Acquire));
        assert_eq!(
            internal.sql("SELECT v FROM t", &[]).unwrap().rows[0]["v"],
            Value::Int(1)
        );
        assert!(!internal.session.statistics_client.load(Ordering::Acquire));
        let pinned = root.open_independent_pinned_read_snapshot().unwrap();
        assert!(pinned.session.statistics_worker.load(Ordering::Acquire));
        assert!(!pinned.session.statistics_client.load(Ordering::Acquire));
        drop(pinned);
        drop(internal);
        let public = root.new_session().unwrap();
        assert!(!public.session.statistics_worker.load(Ordering::Acquire));
        assert!(public.session.statistics_client.load(Ordering::Acquire));
        assert_eq!(
            public.sql("SELECT v FROM t", &[]).unwrap().rows[0]["v"],
            Value::Int(1)
        );
    }
}
