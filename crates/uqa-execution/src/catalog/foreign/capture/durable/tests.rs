//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{catalog_role::RoleIdentity, memory::MemoryBudget, CancellationToken};

fn fixture() -> ForeignTableCaptureSource {
    let first = RelationIdentity::new("Quoted.Schema", "Foreign.Name");
    let second = RelationIdentity::new("public", "pg_temp_local");
    let mut unlogged = super::super::tests::definition();
    unlogged.persistence = RelationPersistence::Unlogged;
    unlogged.object_id = [9; 16];
    let mut security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    security.acl = Some(vec![]);
    security.acl_revisions.relation = Some([3; 16]);
    security
        .acl_revisions
        .columns
        .insert("Removed.Name".into(), [4; 16]);
    ForeignTableCaptureSource {
        tables: Arc::new(BTreeMap::from([
            (first.clone(), super::super::tests::definition()),
            (second.clone(), unlogged),
        ])),
        security: Arc::new(BTreeMap::from([
            (first, BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP)),
            (second, security),
        ])),
    }
}

#[test]
fn durable_foreign_capture_joins_original_authority_before_admitting_selected_payloads() {
    let mut source = fixture();
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let first = source.capture(&control).unwrap();
    let retained = memory.used();
    let mut temporary = super::super::tests::definition();
    temporary.persistence = RelationPersistence::Temporary;
    temporary
        .options
        .insert("payload".into(), "x".repeat(1 << 20));
    Arc::make_mut(&mut source.tables).insert(
        RelationIdentity::new("pg_temp_1", "t".repeat(1 << 20)),
        temporary,
    );
    // Temporary authority is deliberately absent: selection precedes the metadata join.
    let mut saved = None;
    let allocation =
        allocation_counter::measure(|| saved = Some(source.capture(&control).unwrap()));
    let saved = saved.unwrap();
    assert_eq!(memory.used(), retained * 2);
    assert_eq!(allocation.bytes_current, i64::try_from(retained).unwrap());
    assert_eq!(saved.len(), 2);
    for (value, original) in saved.iter().zip(first.iter()) {
        assert_eq!(value.relation, original.relation);
        assert_eq!(value.definition.object_id, original.definition.object_id);
        assert_eq!(
            value.definition.catalog_oids,
            original.definition.catalog_oids
        );
        assert_eq!(
            value.definition.server_reference,
            original.definition.server_reference
        );
        assert_eq!(value.security, original.security);
    }
    assert_eq!(saved[0].security.privileges.acl(), None);
    assert_eq!(saved[1].security.privileges.acl(), Some([].as_slice()));
    assert_eq!(saved[1].security.relation_revision, Some([3; 16]));
    assert_eq!(
        saved[1].security.column_revisions,
        [("Removed.Name".into(), [4; 16])]
    );
    assert_eq!(
        saved[1].definition.persistence,
        RelationPersistence::Unlogged
    );
    drop((source, first, saved));
    assert_eq!(memory.used(), 0);
}

#[test]
fn durable_foreign_capture_rejects_missing_selected_authority_and_releases_partial_output() {
    let mut source = fixture();
    Arc::make_mut(&mut source.security).remove(&RelationIdentity::new("public", "pg_temp_local"));
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    assert!(matches!(
        source.capture(&control),
        Err(SQLError::Internal(_))
    ));
    assert_eq!(memory.used(), 0);
}

#[test]
fn durable_foreign_capture_retains_original_admission_and_independent_cancellation() {
    let source = fixture();
    let control = StorageReadControl::with_limit(64 << 10);
    let captured = source.freeze(&control).unwrap();
    let retained = control.memory().used();
    for limit in [0, 1, retained / 2, retained - 1] {
        let denied = StorageReadControl::with_limit(limit);
        assert!(source.freeze(&denied).is_err());
        assert_eq!(denied.memory().used(), 0);
    }
    let exact = StorageReadControl::with_limit(retained);
    let independent = source.freeze(&exact).unwrap();
    drop(source);
    control.cancellation().cancel();
    assert!(captured.get().is_err());
    assert_eq!(independent.get().unwrap().len(), 2);
    drop((captured, independent));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(exact.memory().used(), 0);
}
