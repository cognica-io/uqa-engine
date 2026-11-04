//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    catalog_role::RoleIdentity, memory::MemoryBudget, CancellationToken, RelationIdentity,
};
use uqa_sql::{ast::RelationPersistence, catalog::security::BoundTableSecurity};

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
    let mut catalog = ForeignCatalog::default();
    catalog
        .insert(
            first,
            super::super::tests::definition(),
            BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP),
        )
        .unwrap();
    catalog.insert(second, unlogged, security).unwrap();
    ForeignTableCaptureSource {
        catalog: Arc::new(catalog),
    }
}

#[test]
fn durable_foreign_capture_retains_paired_roots_without_visiting_temporary_payloads() {
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
    Arc::make_mut(&mut source.catalog)
        .insert(
            RelationIdentity::new("pg_temp_1", "t".repeat(1 << 20)),
            temporary,
            BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP),
        )
        .unwrap();
    let mut saved = None;
    let allocation =
        allocation_counter::measure(|| saved = Some(source.capture(&control).unwrap()));
    let saved = saved.unwrap();
    assert_eq!(allocation.count_total, 0);
    assert_eq!(memory.used(), retained * 2);
    assert_eq!(saved.tables.len(), 2);
    assert_eq!(saved.security.len(), 2);
    for (relation, definition) in saved.tables.iter() {
        assert!(std::ptr::eq(
            definition,
            first.tables.get(relation).unwrap()
        ));
        assert!(std::ptr::eq(
            saved.security.get(relation).unwrap(),
            first.security.get(relation).unwrap()
        ));
    }
    let first_key = RelationIdentity::new("Quoted.Schema", "Foreign.Name");
    let second_key = RelationIdentity::new("public", "pg_temp_local");
    assert_eq!(saved.security.get(&first_key).unwrap().security().acl, None);
    let security = saved.security.get(&second_key).unwrap().security();
    assert_eq!(security.acl, Some(vec![]));
    assert_eq!(security.acl_revisions.relation, Some([3; 16]));
    assert_eq!(
        security.acl_revisions.columns.get("Removed.Name"),
        Some(&[4; 16])
    );
    assert_eq!(
        saved.tables.get(&second_key).unwrap().persistence,
        RelationPersistence::Unlogged
    );
    drop((source, first, saved));
    assert_eq!(memory.used(), 0);
}

#[test]
fn durable_foreign_capture_rejects_failed_pair_admission_without_changing_membership() {
    let memory = MemoryBudget::new(64 << 10);
    let mut catalog = ForeignCatalog::with_budget(&memory);
    let relation = RelationIdentity::new("public", "t");
    catalog
        .insert(
            relation.clone(),
            super::super::tests::definition(),
            BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP),
        )
        .unwrap();
    let before = catalog.clone();
    let used = memory.used();
    let reserved = memory.reserve(memory.available()).unwrap();
    let error = catalog
        .insert(
            relation.clone(),
            super::super::tests::definition(),
            BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP),
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert!(std::ptr::eq(
        catalog.tables().get(&relation).unwrap(),
        before.tables().get(&relation).unwrap()
    ));
    assert!(std::ptr::eq(
        catalog.security().get(&relation).unwrap(),
        before.security().get(&relation).unwrap()
    ));
    drop(reserved);
    assert_eq!(memory.used(), used);
    drop((before, catalog));
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
    assert_eq!(independent.get().unwrap().tables.len(), 2);
    drop((captured, independent));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(exact.memory().used(), 0);
}
