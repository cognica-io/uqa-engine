//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::ProductionControl, CancellationToken, RelationIdentity};

mod bindings;
mod commands;
mod ctes;
mod scheduling;
mod views;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_scope_capture_keeps_catalog_namespace_revision_and_allowance(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("scope-capture.db"));
    engine.sql("CREATE SCHEMA before_path; CREATE SCHEMA after_path; CREATE TABLE before_path.item(id integer); SET search_path = before_path, public", &[]).unwrap();
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory().child(retained.memory().available());
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, retained.cancellation(), &invoking);
    let before = memory.used();
    let scope =
        crate::capabilities::query_scope::new_for_current_routine_with_control(&engine, &control)
            .unwrap();
    assert!(scope.reads_command_overlay());
    let original_revision = scope.catalog_revision().unwrap();
    let original_catalog = scope.catalog_read_view().unwrap();
    assert!(original_catalog
        .snapshot()
        .tables
        .contains_key(&RelationIdentity::new("before_path", "item")));
    engine.sql("DROP TABLE before_path.item; CREATE TABLE after_path.item(value text); SET search_path = after_path, public", &[]).unwrap();
    assert_eq!(
        scope
            .relation_name_resolution_with_control(&control)
            .unwrap()
            .search_path,
        ["before_path", "public"]
    );
    assert!(scope
        .catalog_read_view()
        .unwrap()
        .snapshot()
        .tables
        .contains_key(&RelationIdentity::new("before_path", "item")));
    let current =
        crate::capabilities::query_scope::new_for_current_routine_with_control(&engine, &control)
            .unwrap();
    assert_ne!(current.catalog_revision().unwrap(), original_revision);
    assert_eq!(
        current
            .relation_name_resolution_with_control(&control)
            .unwrap()
            .search_path,
        ["after_path", "public"]
    );
    assert!(current
        .catalog_read_view()
        .unwrap()
        .snapshot()
        .tables
        .contains_key(&RelationIdentity::new("after_path", "item")));
    drop((original_catalog, scope, current));
    assert_eq!(memory.used(), before);
    let occupied = memory.reserve(memory.available() - 32).unwrap();
    assert_eq!(
        crate::capabilities::query_scope::new_for_current_routine_with_control(&engine, &control)
            .err()
            .unwrap()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    invoking.cancel();
    assert_eq!(
        crate::capabilities::query_scope::new_for_current_routine_with_control(&engine, &control)
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
