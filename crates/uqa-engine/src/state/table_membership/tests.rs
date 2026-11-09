//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::Engine;
use uqa_storage::memory_capture::MemoryCapture;

fn sql(engine: &Engine, text: &str) {
    engine.sql(text, &[]).unwrap();
}

fn relation(name: &str) -> RelationIdentity {
    RelationIdentity::new("public", name)
}

fn names(captured: &CapturedTableMembership) -> Vec<String> {
    captured
        .get()
        .unwrap()
        .0
        .sources
        .iter()
        .map(|(name, _)| name.name.clone())
        .collect()
}

fn capture_table(
    membership: &CapturedTableMembership,
    relation: &RelationIdentity,
    cut: &MemoryCapture,
) -> StorageBackendResult<Option<sources::CapturedTable>> {
    let (snapshot, identity) = membership.get()?;
    cut.check()?;
    if !identity.matches(cut) {
        return Err(StorageBackendError::Other(
            "table membership belongs to another memory capture".into(),
        ));
    }
    snapshot
        .sources
        .get(relation)
        .map(|source| source.capture(cut))
        .transpose()
}

#[test]
fn table_membership_preserves_unread_incarnations_without_retaining_live_tables() {
    for mutate in [false, true] {
        let engine = Engine::new();
        sql(&engine, "CREATE TABLE kept(id integer PRIMARY KEY, body text); INSERT INTO kept VALUES(1,'original'); CREATE TABLE moving(id integer); CREATE TEMP TABLE private_temp(value text)");
        let table = engine.table("kept").unwrap().unwrap();
        let weak = Arc::downgrade(&table);
        let object_id = table.object_id();
        let oids = table.recorded_catalog_oids();
        let key = engine.storage.tables.key().unwrap();
        let control = StorageReadControl::with_limit(16 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        drop(table);
        if mutate {
            sql(&engine, "ALTER TABLE moving RENAME TO moved; DROP TABLE kept; CREATE TABLE kept(replacement integer); INSERT INTO kept VALUES(99)");
            assert_ne!(
                engine.table("kept").unwrap().unwrap().object_id(),
                object_id
            );
        }
        drop(engine);
        assert!(weak.upgrade().is_none());
        let membership = key.captured(&cut).unwrap();
        assert_eq!(names(&membership), ["kept", "moving"]);
        let kept = capture_table(&membership, &relation("kept"), &cut)
            .unwrap()
            .unwrap();
        assert_eq!(kept.object_id, object_id);
        assert_eq!(kept.catalog_oids, oids);
        assert_eq!(
            kept.persistence,
            uqa_sql::ast::RelationPersistence::Permanent
        );
        assert_eq!(kept.on_commit, uqa_sql::ast::OnCommitAction::PreserveRows);
        assert_eq!(kept.document_store.len().unwrap(), 1);
        assert_eq!(
            kept.document_store.get_field(1, "body").unwrap(),
            Some(crate::Value::Str("original".into()))
        );
        assert_eq!(
            kept.columns
                .get()
                .unwrap()
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            ["id", "body"]
        );
        assert!(capture_table(&membership, &relation("moved"), &cut)
            .unwrap()
            .is_none());
        let later = MemoryCaptureRegistry::new().capture(&control).unwrap();
        assert!(capture_table(&membership, &relation("kept"), &later).is_err());
        drop((membership, cut, later));
        assert!(control.memory().used() > 0);
        kept.assert_original_allowance(control.memory());
        control.cancellation().cancel();
        assert!(kept.document_store.len().is_err());
        assert!(kept.columns.get().is_err());
        drop(kept);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn table_membership_rollback_restores_the_original_root_without_another_live_map_owner() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE TABLE kept(id integer PRIMARY KEY); INSERT INTO kept VALUES(1)",
    );
    let control = StorageReadControl::with_limit(16 << 20);
    let cut = engine
        .storage
        .memory_capture
        .as_ref()
        .unwrap()
        .capture(&control)
        .unwrap();
    let saved = engine.storage.tables.captured(&cut).unwrap();
    let original_live = engine.storage.tables.0.read().live.identity();
    sql(&engine, "BEGIN");
    sql(&engine, "CREATE TABLE uncommitted(id integer); SAVEPOINT point; DROP TABLE kept; CREATE TABLE kept(other text); ROLLBACK TO point; ROLLBACK");
    assert_eq!(
        engine
            .storage
            .tables
            .read()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        [relation("kept")]
    );
    assert!(!original_live.is_alive());
    let restored_live;
    {
        let state = engine.storage.tables.0.read();
        let live = state
            .memory
            .as_ref()
            .unwrap()
            .membership
            .sources
            .get(&relation("kept"))
            .unwrap();
        assert!(std::ptr::eq(
            live,
            saved
                .get()
                .unwrap()
                .0
                .sources
                .get(&relation("kept"))
                .unwrap()
        ));
        restored_live = state.live.identity();
    }
    assert_eq!(names(&saved), ["kept"]);
    assert_eq!(
        capture_table(&saved, &relation("kept"), &cut)
            .unwrap()
            .unwrap()
            .document_store
            .len()
            .unwrap(),
        1
    );
    drop(engine);
    assert!(!restored_live.is_alive());
    drop((saved, cut));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn table_membership_capture_does_not_copy_the_relation_corpus_or_temporary_payloads() {
    let mut allocations = Vec::new();
    for count in [1, 128] {
        let engine = Engine::new();
        for id in 0..count {
            engine
                .create_default_table(format!("table_{id}"), vec![])
                .unwrap();
        }
        sql(&engine, "CREATE TEMP TABLE private_temp(value text)");
        let temporary = engine.table("private_temp").unwrap().unwrap();
        temporary
            .columns
            .write()
            .edit(|columns| {
                columns[0].missing_value = Some(crate::Value::Str("x".repeat(1 << 20)));
                Ok(())
            })
            .unwrap();
        drop(temporary);
        let control = StorageReadControl::with_limit(16 << 20);
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            let cut = engine
                .storage
                .memory_capture
                .as_ref()
                .unwrap()
                .capture(&control)
                .unwrap();
            let membership = engine.storage.tables.captured(&cut).unwrap();
            result = Some((cut, membership));
        });
        allocations.push(allocation.bytes_total);
        let (cut, membership) = result.unwrap();
        assert_eq!(membership.get().unwrap().0.sources.len(), count);
        assert!(membership.get().unwrap().0.payload_bytes < (1 << 20));
        assert!(names(&membership)
            .iter()
            .all(|name| name.starts_with("table_")));
        drop((cut, membership, engine));
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(allocations[0], allocations[1]);
    assert!(allocations[1] < 4096, "{allocations:?}");
}

#[test]
fn denied_table_membership_retention_does_not_consume_publication_or_an_independent_cut() {
    let engine = Engine::new();
    engine.create_default_table("kept", vec![]).unwrap();
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let small = StorageReadControl::with_limit(1024);
    let good = StorageReadControl::with_limit(16 << 20);
    let denied = registry.capture(&small).unwrap();
    let cut = registry.capture(&good).unwrap();
    assert!(engine.storage.tables.captured(&denied).is_err());
    let membership = engine.storage.tables.captured(&cut).unwrap();
    engine.create_default_table("later", vec![]).unwrap();
    assert!(engine
        .storage
        .tables
        .read()
        .contains_key(&relation("later")));
    assert_eq!(names(&membership), ["kept"]);
    assert!(denied.check().is_err());
    assert!(cut.check().is_ok());
    good.cancellation().cancel();
    assert!(capture_table(&membership, &relation("kept"), &cut).is_err());
    drop((membership, denied, cut, engine));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}

#[test]
fn persistent_table_membership_retention_keeps_unique_maps_in_place() {
    let engine = Engine::new();
    let registry = TableRegistry::persistent();
    for id in 0..128 {
        let name = format!("table_{id}");
        engine.create_default_table(&name, vec![]).unwrap();
        registry
            .write()
            .insert(relation(&name), engine.table(&name).unwrap().unwrap())
            .unwrap();
    }
    let mut visited = 0;
    let allocation = allocation_counter::measure(|| {
        registry
            .write()
            .retain(|relation, _| {
                visited += 1;
                relation.name != "table_0"
            })
            .unwrap();
    });
    assert_eq!(visited, 128);
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
    let retained = registry.read();
    assert_eq!(retained.len(), 127);
    assert!(!retained.contains_key(&relation("table_0")));
    for (name, table) in retained.iter() {
        assert!(Arc::ptr_eq(
            table,
            engine.storage.tables.read().get(name).unwrap()
        ));
    }
}
