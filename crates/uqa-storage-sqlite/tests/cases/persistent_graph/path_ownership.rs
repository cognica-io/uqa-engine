//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{catalog::path_index, GraphSnapshot, PersistentStorageProvider};

fn verify(catalog: &dyn CatalogFacade, backend: &dyn PersistentStorageBackend) {
    let parent = "company";
    let child = "company::archive";
    let lookalike = format!("{}ordinary", path_index::prefixes(parent).1);
    for graph in [parent, child, lookalike.as_str()] {
        catalog.save_named_graph(graph).unwrap();
    }
    let parent_index = path_index::key(parent, "archive::idx");
    let child_index = path_index::key(child, "idx");
    let literal = path_index::key(&lookalike, "idx");
    let legacy = "company::archive::recorded";
    let ambiguous = "company::archive::unrecorded";
    for (key, graph) in [
        (parent_index.as_str(), parent),
        (child_index.as_str(), child),
        (&literal, &lookalike),
        (legacy, child),
        ("opaque", parent),
        ("company::plain", parent),
    ] {
        catalog.save_path_index(key, "[[\"edge\"]]").unwrap();
        catalog
            .save_path_index_pairs(key, "[\"edge\"]", &[(10, 11)])
            .unwrap();
        catalog
            .finish_path_index_data(key, graph, "[[\"edge\"]]")
            .unwrap();
        assert_eq!(
            catalog.path_index_graph(key).unwrap().as_deref(),
            Some(graph)
        );
    }
    // Invalidation retires the numerical result while retaining the independently known owner.
    catalog.save_path_index(legacy, "[]").unwrap();
    assert!(!catalog.path_index_data_is_current(legacy, "[]").unwrap());
    assert_eq!(
        catalog.path_index_graph(legacy).unwrap().as_deref(),
        Some(child)
    );
    catalog.save_path_index(ambiguous, "[]").unwrap();
    assert!(catalog.path_index_graph(ambiguous).unwrap().is_none());
    let definitions = || {
        catalog
            .load_path_indexes()
            .unwrap()
            .into_iter()
            .map(|(key, _)| key)
            .collect::<BTreeSet<_>>()
    };
    let all = definitions();
    let replacement = GraphSnapshot {
        vertices: Vec::new(),
        edges: Vec::new(),
        label_registry_json: "{}".into(),
    };
    for mode in 0..3 {
        let reject = match mode {
            0 => catalog.drop_graph_path_indexes(parent),
            1 => catalog.replace_named_graph(parent, &replacement),
            _ => catalog.drop_named_graph_data(parent),
        }
        .unwrap_err();
        assert!(
            reject.to_string().contains("ambiguous path-index owner"),
            "{reject}"
        );
        assert_eq!(definitions(), all);
        assert!(catalog.named_graph_exists(parent).unwrap());
    }
    catalog.drop_path_index(ambiguous).unwrap();
    let before = definitions();
    let keep = BTreeSet::from([
        child_index.clone(),
        literal.clone(),
        legacy.to_owned(),
        String::from("opaque"),
    ]);
    for mode in 0..3 {
        backend.begin_transaction().unwrap();
        match mode {
            0 => catalog.drop_graph_path_indexes(parent),
            1 => catalog.replace_named_graph(parent, &replacement),
            _ => catalog.drop_named_graph_data(parent),
        }
        .unwrap();
        assert_eq!(definitions(), keep);
        assert_eq!(
            catalog.path_index_graph(&child_index).unwrap().as_deref(),
            Some(child)
        );
        assert_eq!(
            catalog
                .path_index_pairs(&child_index, "[\"edge\"]", None, 2)
                .unwrap(),
            [(10, 11)]
        );
        backend.rollback_transaction().unwrap();
        assert_eq!(definitions(), before);
        assert!(catalog.named_graph_exists(parent).unwrap());
    }
    catalog.clear_path_index_data(legacy).unwrap();
    assert!(catalog.path_index_graph(legacy).unwrap().is_none());
}

#[test]
fn graph_scoped_removal_uses_recorded_owners_and_rolls_back_ambiguous_legacy_changes() {
    for (catalog, backend) in fixtures() {
        verify(catalog.as_ref(), backend.as_ref());
    }
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = uqa_storage_sqlite::Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    verify(&catalog, &SQLiteStorageBackend::new(connection));
    let directory = tempfile::tempdir().unwrap();
    let provider = uqa_storage_sqlite::SQLiteKeyValueStorage::open(
        &directory.path().join("graph-key-value.db"),
    )
    .unwrap();
    let session = provider.open_session().unwrap();
    verify(session.catalog.as_ref(), session.backend.as_ref());
}
