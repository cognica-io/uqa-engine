//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::catalog::roles::RoleIdentity;

fn table(name: &str, persistence: RelationPersistence) -> StoredForeignTable {
    let mut table = super::super::capture::tests::definition();
    table.name = name.to_owned();
    table.persistence = persistence;
    table
}

fn authority() -> BoundTableSecurity {
    let mut security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    security.acl = Some(vec![]);
    security.acl_revisions.relation = Some([7; 16]);
    security
        .acl_revisions
        .columns
        .insert("Removed.Name".into(), [8; 16]);
    security
}

#[test]
fn native_foreign_capture_is_constant_allocation_and_retains_complete_old_pairs() {
    for count in [1, 64, 4096] {
        let native = MemoryBudget::new(128 << 20);
        let mut source = ForeignCatalog::with_budget(&native);
        for index in 0..count {
            source
                .insert(
                    RelationIdentity::new("public", format!("t{index:05}")),
                    table("saved", RelationPersistence::Permanent),
                    authority(),
                )
                .unwrap();
        }
        let first = RelationIdentity::new("public", "t00000");
        let temporary = RelationIdentity::new("public", "temporary");
        let mut large = table("temporary", RelationPersistence::Temporary);
        large.options.insert("large".into(), "x".repeat(1 << 20));
        source.insert(temporary, large, authority()).unwrap();
        let memory = MemoryBudget::new(128 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut saved = None;
        let allocation =
            allocation_counter::measure(|| saved = Some(source.capture(&control).unwrap()));
        assert_eq!(allocation.count_total, 0);
        let saved = saved.unwrap();
        assert_eq!(saved.tables.len(), count);
        assert_eq!(saved.security.len(), count);
        assert!(std::ptr::eq(
            saved.tables.get(&first).unwrap(),
            source.tables().get(&first).unwrap()
        ));
        assert!(std::ptr::eq(
            saved.security.get(&first).unwrap().security(),
            source.security().get(&first).unwrap()
        ));
        let required = memory.used();
        let denied = MemoryBudget::new(required - 1);
        assert!(
            matches!(source.capture(&ProductionControl::new(&denied, &token, &token)), Err(error) if error.sqlstate() == Some("53200"))
        );
        assert_eq!(denied.used(), 0);
        let exact = MemoryBudget::new(required);
        let independent = source
            .capture(&ProductionControl::new(&exact, &token, &token))
            .unwrap();
        let mut replacement = table("replacement", RelationPersistence::Unlogged);
        replacement.object_id = [55; 16];
        source
            .insert(
                first.clone(),
                replacement,
                BoundTableSecurity::owner(RoleIdentity {
                    oid: 42_000,
                    object_id: [42; 16],
                }),
            )
            .unwrap();
        assert_eq!(saved.tables.get(&first).unwrap().name, "saved");
        assert_eq!(saved.security.get(&first).unwrap().security(), &authority());
        let original = table("saved", RelationPersistence::Permanent);
        let captured = saved.tables.get(&first).unwrap();
        assert_eq!(
            captured.schema_json().unwrap(),
            original.schema_json().unwrap()
        );
        assert_eq!(captured.options, original.options);
        assert!(captured.columns[1]
            .missing_value
            .as_ref()
            .unwrap()
            .has_same_representation(original.columns[1].missing_value.as_ref().unwrap()));
        drop(source);
        drop(saved);
        assert_eq!(memory.used(), 0);
        assert!(native.used() > 0);
        drop(independent);
        assert_eq!(exact.used(), 0);
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn native_foreign_changes_share_unrelated_payloads_and_publish_both_roots_on_success() {
    let memory = MemoryBudget::new(16 << 20);
    let mut source = ForeignCatalog::with_budget(&memory);
    for index in 0..128 {
        source
            .insert(
                RelationIdentity::new("public", format!("t{index:03}")),
                table("original", RelationPersistence::Permanent),
                authority(),
            )
            .unwrap();
    }
    let large_key = RelationIdentity::new("public", "unrelated");
    let mut large = table("large", RelationPersistence::Permanent);
    large.options.insert("opaque".into(), "x".repeat(4 << 20));
    source
        .insert(large_key.clone(), large, authority())
        .unwrap();
    let original = source.clone();
    let selected = RelationIdentity::new("public", "t064");
    let allocated = allocation_counter::measure(|| {
        source
            .insert(
                selected.clone(),
                table("changed", RelationPersistence::Temporary),
                authority(),
            )
            .unwrap();
        source
            .remove(&RelationIdentity::new("public", "t001"))
            .unwrap();
    });
    assert!(allocated.bytes_total < 64 << 10, "{allocated:?}");
    assert!(allocated.count_total < 256, "{allocated:?}");
    assert!(std::ptr::eq(
        source.tables().get(&large_key).unwrap(),
        original.tables().get(&large_key).unwrap()
    ));
    assert_eq!(original.tables()[&selected].name, "original");
    assert_eq!(source.tables()[&selected].name, "changed");
    let captured_budget = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let captured = source
        .capture(&ProductionControl::new(&captured_budget, &token, &token))
        .unwrap();
    assert!(captured.tables.get(&selected).is_none());
    assert!(captured.security.get(&selected).is_none());
    let previous = source.clone();
    let bytes = memory.used();
    let blocked = memory.reserve(memory.available() - (16 << 10)).unwrap();
    let mut denied = table("denied", RelationPersistence::Permanent);
    denied.options.insert("large".into(), "x".repeat(1 << 20));
    assert_eq!(
        source
            .insert(selected.clone(), denied, authority())
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert!(Arc::ptr_eq(
        &source.tables_snapshot(),
        &previous.tables_snapshot()
    ));
    assert!(Arc::ptr_eq(
        &source.security_snapshot(),
        &previous.security_snapshot()
    ));
    drop(blocked);
    assert_eq!(memory.used(), bytes);
    let blocked = memory.reserve(memory.available()).unwrap();
    assert_eq!(
        source
            .remove(&RelationIdentity::new("public", "t002"))
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert!(Arc::ptr_eq(
        &source.tables_snapshot(),
        &previous.tables_snapshot()
    ));
    assert!(Arc::ptr_eq(
        &source.security_snapshot(),
        &previous.security_snapshot()
    ));
    drop(blocked);
    drop((source, previous, original, captured));
    assert_eq!(memory.used(), 0);
    assert_eq!(captured_budget.used(), 0);
}
