//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use uqa_core::RelationIdentity;
use uqa_storage::{read_control::StorageReadControl, vector_index::MemoryVectorIndex};

#[test]
fn denied_live_table_publication_preserves_members_and_child_update_addresses() {
    let engine = Engine::new();
    for name in ["kept", "second", "detached"] {
        engine.create_default_table(name, vec![]).unwrap();
    }
    let name = |value| RelationIdentity::new("public", value);
    let detached = engine
        .storage
        .tables
        .write()
        .remove(&name("detached"))
        .unwrap()
        .unwrap();
    let memory = engine
        .storage
        .tables
        .0
        .read()
        .memory
        .as_ref()
        .unwrap()
        .publication
        .memory
        .clone();
    let occupied = memory.reserve(memory.available()).unwrap();
    assert!(engine
        .storage
        .tables
        .write()
        .insert(name("detached"), detached.clone())
        .is_err());
    assert!(engine.storage.tables.write().remove(&name("kept")).is_err());
    let replacement = engine.storage.tables.read().clone();
    assert!(engine.storage.tables.write().replace(replacement).is_err());
    engine
        .storage
        .tables
        .write()
        .rename(&name("kept"), name("renamed"))
        .unwrap()
        .unwrap();
    drop(occupied);
    let table = engine.table("renamed").unwrap().unwrap();
    table
        .vector_indexes
        .write()
        .live_mut()
        .unwrap()
        .insert("added".into(), Box::new(MemoryVectorIndex::new(2)))
        .unwrap();
    let control = StorageReadControl::with_limit(4 << 20);
    let selected = engine.capture_committed_memory(&control).unwrap();
    assert_eq!(selected.names().unwrap(), [name("renamed"), name("second")]);
    assert!(selected
        .table(&name("renamed"))
        .unwrap()
        .vector_indexes
        .image("added", &selected.cut)
        .is_ok());
    assert!(detached.vector_indexes.captured(&selected.cut).is_err());
    engine
        .storage
        .tables
        .write()
        .insert(name("detached"), detached.clone())
        .unwrap();
    detached
        .vector_indexes
        .write()
        .live_mut()
        .unwrap()
        .insert("later".into(), Box::new(MemoryVectorIndex::new(3)))
        .unwrap();
    let later = engine.capture_committed_memory(&control).unwrap();
    assert_eq!(
        later.names().unwrap(),
        [name("detached"), name("renamed"), name("second")]
    );
    assert!(later
        .table(&name("detached"))
        .unwrap()
        .vector_indexes
        .image("later", &later.cut)
        .is_ok());
    drop((selected, later, detached, table, engine));
    assert_eq!(memory.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case(Provider::SQLiteNative)]
#[case(Provider::SQLiteKeyValue)]
#[case(Provider::Redb)]
fn persistent_registry_restore_token_cannot_admit_memory_source_restoration(
    #[case] provider: Provider,
) {
    let engine = Engine::new();
    engine.create_default_table("kept", vec![]).unwrap();
    assert!(engine.storage.tables.persistent_snapshot().is_err());
    assert_eq!(engine.table_names().unwrap(), ["public.kept"]);
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("registry.db"));
    engine.create_default_table("kept", vec![]).unwrap();
    let snapshot = engine.storage.tables.persistent_snapshot().unwrap();
    engine
        .storage
        .tables
        .write()
        .remove(&RelationIdentity::new("public", "kept"))
        .unwrap();
    assert!(engine.storage.tables.read().is_empty());
    snapshot.restore();
    assert!(std::sync::Arc::ptr_eq(
        &engine.storage.tables.read()[&RelationIdentity::new("public", "kept")],
        &snapshot.tables()[&RelationIdentity::new("public", "kept")]
    ));
    assert_eq!(engine.table_names().unwrap(), ["public.kept"]);
}
