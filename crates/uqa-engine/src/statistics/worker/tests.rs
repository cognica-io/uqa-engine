//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Idle maintenance must not fetch record payloads or scan the catalog again.

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteStorageBackend};

use super::*;

#[test]
fn unchanged_statistics_poll_needs_no_catalog_records() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let engine = Engine::from_persistent_backends(
        Arc::new(catalog),
        Arc::new(SQLiteStorageBackend::new(connection.clone())),
    )
    .unwrap();
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    engine.sql("CREATE TABLE items (id INTEGER); CREATE TABLE events (id INTEGER); ANALYZE items; ANALYZE events", &[]).unwrap();
    let mut cache = cache::MaintenanceCache::default();
    refresh_due_tables(&engine, &mut cache).unwrap();
    connection
        .with_physical(|sqlite| {
            sqlite.flush_prepared_statement_cache();
            sqlite.authorizer(Some(|context: AuthContext<'_>| match context.action {
                AuthAction::Read {
                    table_name: "_uqa_mvcc_heads" | "_uqa_mvcc_versions" | "_uqa_mvcc_runs",
                    ..
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }))?;
            Ok(())
        })
        .unwrap();
    for _ in 0..4 {
        refresh_due_tables(&engine, &mut cache).unwrap();
    }
    connection
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
            Ok(())
        })
        .unwrap();
    engine.sql("INSERT INTO items VALUES (1)", &[]).unwrap();
    refresh_due_tables(&engine, &mut cache).unwrap();
    assert_eq!(
        engine
            .sql("SELECT count(*) AS n FROM items", &[])
            .unwrap()
            .rows[0]["n"],
        uqa_core::Value::Int(1)
    );
}
