//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::session::Database;
use std::sync::Arc;
use uqa_core::memory::MemoryBudget;
use uqa_graph::{GraphCatalog, GraphStoreHandle, PathIndexCatalog, PathIndexCatalogInput};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, PersistentStorageSession,
    StorageBackendError,
};

fn open(
    session: &PersistentStorageSession,
    mutations: &MemoryBudget,
    control: &StorageReadControl,
) -> PathIndexCatalogInput {
    let mut graphs = GraphCatalog::new(mutations);
    graphs
        .insert("g", Arc::new(GraphStoreHandle::default()))
        .unwrap();
    PathIndexCatalogInput::new(
        graphs,
        session.catalog.clone(),
        session.backend.clone(),
        mutations.clone(),
        CatalogInputLimits {
            maximum_key_bytes: 4096,
            maximum_value_bytes: 4096,
        },
        control,
    )
    .unwrap()
}

#[test]
fn private_durable_path_catalog_binds_complete_generations_in_bounded_windows() {
    let database = Database::new();
    let session = database.session();
    session.backend.begin_transaction().unwrap();
    session.catalog.save_named_graph("g").unwrap();
    for n in 0..4096 {
        session
            .catalog
            .save_path_index(&format!("g::p{n:04}"), r#"[["old"]]"#)
            .unwrap();
    }
    session.backend.commit_transaction().unwrap();
    session.backend.begin_read_transaction().unwrap();
    let mutations = MemoryBudget::new(4 << 20);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut reader = open(&session, &mutations, &control);
    assert!(reader.advance(1).unwrap().is_none());
    let newer = database.session();
    newer
        .catalog
        .save_path_index("g::p4095", r#"[["new"]]"#)
        .unwrap();
    let mut calls = 1;
    let paths = loop {
        calls += 1;
        if let Some(paths) = reader.advance(1).unwrap() {
            break paths;
        }
    };
    assert!(
        calls > 4096,
        "{calls} single-step polls for 4096 registrations"
    );
    assert_eq!(paths.len(), 4096);
    assert!(paths.budget().shares_allowance(&mutations));
    let index = paths.get("g::p4095").unwrap().clone();
    assert_eq!(index.physical_key(), Some("g::p4095"));
    assert!(index.has_path(&["old".into()]));
    assert!(session.backend.in_transaction());
    drop((reader, paths));
    assert!(
        control.memory().used() > 0,
        "the physical handle must retain its original definition"
    );
    let rebound = index
        .rebind_persistent(newer.catalog.clone(), newer.backend.clone())
        .unwrap();
    let view = index.with_graph_read_view(Arc::default());
    session.backend.rollback_transaction().unwrap();
    drop(index);
    assert!(rebound.has_path(&["old".into()]) && view.has_path(&["old".into()]));
    drop(rebound);
    assert!(control.memory().used() > 0);
    drop(view);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(mutations.used(), 0);

    for cancelled in [false, true] {
        session.backend.begin_read_transaction().unwrap();
        let control = StorageReadControl::with_limit(32 << 20);
        let original = PathIndexCatalog::new(&mutations);
        let mut input = open(&session, &mutations, &control);
        // Reach the final registrations after ordered source loading has completed. Both declarations have the same length and label count on the newer cut.
        for _ in 0..(calls - 512) / 32 {
            assert!(input.advance(32).unwrap().is_none());
        }
        let held = if cancelled {
            control.cancellation().cancel();
            None
        } else {
            Some(
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap(),
            )
        };
        let error = loop {
            match input.advance(1) {
                Ok(None) => (),
                Ok(Some(_)) => panic!("unsuccessful private paths must remain unpublished"),
                Err(error) => break error,
            }
        };
        assert!(if cancelled {
            matches!(error, StorageBackendError::Cancelled(_))
        } else {
            matches!(error, StorageBackendError::Memory(_))
        });
        assert!(original.is_empty());
        assert!(session.backend.in_transaction());
        drop((held, input, original));
        assert_eq!(control.memory().used(), 0);
        assert_eq!(mutations.used(), 0);
        session.backend.rollback_transaction().unwrap();
    }
}
