//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn name(text: &str, control: &StorageReadControl) -> StorageName {
    let mut value = BudgetedString::new(control.memory());
    value.push_str(text).unwrap();
    let (value, memory) = value.into_parts();
    StorageName::from_admitted(Budgeted::new(value, memory)).unwrap()
}

#[test]
fn redb_vector_catalog_collections_preserve_committed_rows_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vector-catalog.redb");
    let storage = crate::RedbStorage::open(&path).unwrap();
    let backend = Arc::new(KeyValueStorageBackend::new(Arc::new(storage.store())));
    let control = StorageReadControl::with_limit(1 << 20);
    let fields = uqa_storage::catalog::input::table::vector_fields(
        br#"[{"field":"embedding","dimensions":2}]"#,
        &control,
    )
    .unwrap();
    let mut input = uqa_storage::vector_index::PersistentVectorIndexesBuilder::catalog_input(
        backend.clone(),
        name("public.notes", &control),
        fields,
        &control,
    )
    .unwrap();
    let mut advances = 0;
    let mut indexes = loop {
        advances += 1;
        assert!(advances < 128);
        if let Some(value) = input
            .advance(
                &mut std::io::empty(),
                uqa_core::image_io::ImageDecodeWindow { bytes: 1, steps: 1 },
            )
            .unwrap()
        {
            break value;
        }
    };
    drop(input);
    control.cancellation().cancel();
    indexes
        .live_mut()
        .unwrap()
        .get_mut("embedding")
        .unwrap()
        .add(1, vec![1.0, 0.0])
        .unwrap();
    backend.begin_transaction().unwrap();
    indexes
        .live_mut()
        .unwrap()
        .get_mut("embedding")
        .unwrap()
        .add(2, vec![0.0, 1.0])
        .unwrap();
    backend.rollback_transaction().unwrap();
    drop((indexes, backend, storage));
    assert_eq!(control.memory().used(), 0);
    let storage = crate::RedbStorage::open(&path).unwrap();
    let backend = KeyValueStorageBackend::new(Arc::new(storage.store()));
    let index = backend
        .vector_index(
            "public.notes",
            "embedding",
            2,
            uqa_storage::VectorIndexSpec::BruteForce,
            uqa_storage::VectorIndexOpenMode::Restore,
        )
        .unwrap();
    assert_eq!(index.count().unwrap(), 1);
    assert!(index.contains_document(1).unwrap());
    assert!(!index.contains_document(2).unwrap());
}

#[test]
fn redb_controlled_vector_handles_preserve_committed_records_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("canonical-vectors.redb");
    let storage = crate::RedbStorage::open(&path).unwrap();
    let backend = KeyValueStorageBackend::new(Arc::new(storage.store()));
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = backend
        .canonical_vector_index_with_control(
            name("public.notes", &control),
            name("embedding", &control),
            2,
            &control,
        )
        .unwrap();
    control.cancellation().cancel();
    index.add(1, vec![1.0, 0.0]).unwrap();
    backend.begin_transaction().unwrap();
    index.add(2, vec![0.0, 1.0]).unwrap();
    backend.rollback_transaction().unwrap();
    assert_eq!(index.count().unwrap(), 1);
    assert!(!index.contains_document(2).unwrap());
    drop((index, backend, storage));
    assert_eq!(control.memory().used(), 0);
    let storage = crate::RedbStorage::open(&path).unwrap();
    let backend = KeyValueStorageBackend::new(Arc::new(storage.store()));
    let index = backend
        .vector_index(
            "public.notes",
            "embedding",
            2,
            uqa_storage::VectorIndexSpec::BruteForce,
            uqa_storage::VectorIndexOpenMode::Restore,
        )
        .unwrap();
    assert_eq!(index.count().unwrap(), 1);
    let hits = index.search_knn(&[1.0, 0.0], 2).unwrap();
    assert_eq!(hits.entries().len(), 1);
    assert_eq!(hits.entries()[0].doc_id, 1);
}
