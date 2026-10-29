//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::ProductionControl, CancellationToken};
use uqa_execution::catalog::services::CatalogSession;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_namespace_capture_admits_before_snapshot_and_keeps_session_values(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("namespace-capture.db"));
    let path = (0..4096)
        .map(|index| format!("missing_{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    engine
        .sql(
            &format!("SET search_path TO \"$user\", public, {path}"),
            &[],
        )
        .unwrap();
    let expected = CatalogSession::relation_name_resolution(&engine);
    assert_eq!(expected.search_path.len(), 4098);
    assert_eq!(expected.search_path[0], "uqa");
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(memory, resources.cancellation(), &invoking);
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - 512).unwrap();
    let allocation = allocation_counter::measure(|| {
        let error = CatalogSession::relation_name_resolution_with_control(&engine, &control)
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
    });
    assert!(
        allocation.bytes_total < 16 * 1024,
        "{provider:?}: {allocation:?}"
    );
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    let captured =
        CatalogSession::relation_name_resolution_with_control(&engine, &control).unwrap();
    assert_eq!(captured.search_path, expected.search_path);
    assert_eq!(captured.temporary_schema, expected.temporary_schema);
    assert_eq!(captured.current_user, expected.current_user);
    assert_eq!(
        captured.temporary_namespace_allocated,
        expected.temporary_namespace_allocated
    );
    assert_eq!(memory.used(), before + captured.reserved_bytes());
    drop(captured);
    assert_eq!(memory.used(), before);
    invoking.cancel();
    assert_eq!(
        CatalogSession::relation_name_resolution_with_control(&engine, &control)
            .err()
            .unwrap()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(memory.used(), before);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
