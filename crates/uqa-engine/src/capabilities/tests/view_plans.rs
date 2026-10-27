//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read retained view definitions without copying their plans or holding registry locks through recursive readers.

use crate::tests::providers::Provider;
use crate::{Engine, RelationIdentity};
use std::sync::atomic::Ordering;
use uqa_sql::{semantics::volatility::VolatilityCatalog, SQLError};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn view_plan_readers_keep_native_snapshot_identity_and_release_registry_locks(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let mut engine = provider.open(&directory.path().join("views.db"));
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    engine
        .sql(
            "CREATE VIEW borrowed_leaf AS SELECT 41 AS value;
             CREATE VIEW borrowed_parent AS SELECT * FROM borrowed_leaf;
             CREATE MATERIALIZED VIEW borrowed_materialized AS SELECT 7 AS value",
            &[],
        )
        .unwrap();

    verify_readers(&engine);
    let retained = engine.durable.views.snapshot();
    engine
        .sql(
            "CREATE OR REPLACE VIEW borrowed_leaf AS SELECT 42 AS value",
            &[],
        )
        .unwrap();
    let relation = RelationIdentity::new("public", "borrowed_leaf");
    assert_ne!(
        serde_json::to_value(&retained[&relation].definition.query).unwrap(),
        serde_json::to_value(&engine.durable.views.snapshot()[&relation].definition.query).unwrap(),
    );
    verify_readers(&engine);

    // Retained portal adapters select their captured registry, even after the live definition changes.
    engine.query_view_snapshots = Some(retained);
    verify_readers(&engine);
    engine.query_view_snapshots = None;
    verify_readers(&engine);
}

fn verify_readers(engine: &Engine) {
    engine
        .with_direct_read_snapshot(|reader| {
            let snapshot = reader
                .query_view_snapshots
                .clone()
                .unwrap_or_else(|| reader.durable.views.snapshot());
            let parent = RelationIdentity::new("public", "borrowed_parent");
            let leaf = RelationIdentity::new("public", "borrowed_leaf");
            assert_eq!(
                reader.with_view_query("borrowed_parent", &mut |query| {
                    assert!(std::ptr::eq(
                        query,
                        &raw const snapshot[&parent].definition.query
                    ));
                    assert!(!reader.durable.views.is_locked());
                    assert_eq!(
                        reader.with_view_query("borrowed_leaf", &mut |child| {
                            assert!(std::ptr::eq(
                                child,
                                &raw const snapshot[&leaf].definition.query
                            ));
                            assert!(!reader.durable.views.is_locked());
                            Ok(false)
                        })?,
                        Some(false),
                    );
                    Ok(true)
                })?,
                Some(true),
            );
            for name in ["missing_view", "borrowed_materialized"] {
                assert_eq!(
                    reader.with_view_query(name, &mut |_| panic!("no ordinary view"))?,
                    None,
                );
            }
            let error = reader
                .with_view_query("borrowed_leaf", &mut |_| {
                    Err(SQLError::Internal("view reader failed".into()))
                })
                .unwrap_err();
            assert_eq!(error.to_string(), "internal error: view reader failed");
            assert!(!reader.durable.views.is_locked());
            assert!(reader
                .with_view_query("borrowed_leaf", &mut |_| Ok(true))?
                .unwrap());
            Ok(())
        })
        .unwrap();
}
