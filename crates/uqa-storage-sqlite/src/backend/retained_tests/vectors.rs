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
fn sqlite_vector_catalog_collections_preserve_writes_and_rollback_after_input_retirement() {
    for mode in ["native", "kv", "legacy"] {
        let backend = backend(mode);
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
        assert_eq!(indexes["embedding"].count().unwrap(), 1, "{mode}");
        assert!(
            !indexes["embedding"].contains_document(2).unwrap(),
            "{mode}"
        );
        assert_eq!(
            indexes["embedding"]
                .search_knn(&[1.0, 0.0], 1)
                .unwrap()
                .entries()[0]
                .doc_id,
            1,
            "{mode}"
        );
        drop(indexes);
        assert_eq!(control.memory().used(), 0, "{mode}");
    }
}

#[test]
fn sqlite_controlled_vector_handles_preserve_native_reads_and_rollback() {
    for mode in ["native", "kv", "legacy"] {
        let backend = backend(mode);
        let control = StorageReadControl::with_limit(1 << 20);
        let mut index = backend
            .canonical_vector_index_with_control(
                name("public.notes", &control),
                name("embedding", &control),
                2,
                &control,
            )
            .unwrap();
        let retained = control.memory().used();
        assert!(retained > 0, "{mode}");
        control.cancellation().cancel();
        index.add(1, vec![1.0, 0.0]).unwrap();
        backend.begin_transaction().unwrap();
        index.add(2, vec![0.0, 1.0]).unwrap();
        assert_eq!(
            index.search_knn(&[1.0, 0.0], 2).unwrap().entries().len(),
            2,
            "{mode}"
        );
        backend.rollback_transaction().unwrap();
        assert_eq!(index.count().unwrap(), 1, "{mode}");
        assert!(!index.contains_document(2).unwrap(), "{mode}");
        let hits = index.search_knn(&[1.0, 0.0], 2).unwrap();
        assert_eq!(hits.entries().len(), 1, "{mode}");
        assert_eq!(hits.entries()[0].doc_id, 1, "{mode}");
        assert_eq!(control.memory().used(), retained, "{mode}");
        drop(index);
        assert_eq!(control.memory().used(), 0, "{mode}");
        let index = backend
            .vector_index(
                "public.notes",
                "embedding",
                2,
                VectorIndexSpec::BruteForce,
                VectorIndexOpenMode::Restore,
            )
            .unwrap();
        assert_eq!(index.count().unwrap(), 1, "{mode}");
        assert_eq!(
            index.search_knn(&[1.0, 0.0], 1).unwrap().entries()[0].doc_id,
            1,
            "{mode}"
        );
    }
}

#[test]
fn sqlite_vector_snapshots_keep_original_shared_handle_allowances() {
    for mode in ["native", "legacy"] {
        let backend = backend(mode);
        let control = StorageReadControl::with_limit(1 << 20);
        let mut index = backend
            .canonical_vector_index_with_control(
                name("public.notes", &control),
                name("embedding", &control),
                2,
                &control,
            )
            .unwrap();
        index.add(1, vec![1.0, 0.0]).unwrap();
        let bytes = control.memory().used();
        let snapshot = index.snapshot().unwrap();
        control.cancellation().cancel();
        drop(index);
        assert_eq!(
            control.memory().used(),
            bytes - size_of::<SQLiteVectorIndex>(),
            "{mode}"
        );
        assert_eq!(snapshot.count().unwrap(), 1, "{mode}");
        let hits = snapshot.search_knn(&[1.0, 0.0], 1).unwrap();
        assert_eq!(hits.entries()[0].doc_id, 1, "{mode}");
        assert_eq!(hits.entries()[0].payload.score, 1.0, "{mode}");
        drop(snapshot);
        assert_eq!(control.memory().used(), 0, "{mode}");
    }
}

#[test]
fn sqlite_controlled_vector_handle_failures_retire_names_and_cache() {
    for mode in ["native", "kv", "legacy"] {
        let backend = backend(mode);
        for cancel in [false, true] {
            for spare in [
                0,
                if mode == "kv" {
                    1
                } else {
                    size_of::<SQLiteVectorIndex>()
                },
            ] {
                let control = StorageReadControl::with_limit(1 << 20);
                let table = name("public.notes", &control);
                let field = name("embedding", &control);
                let block = control
                    .memory()
                    .reserve(control.memory().available() - spare)
                    .unwrap();
                let before = control.memory().used();
                if cancel {
                    control.cancellation().cancel();
                }
                let error = backend
                    .canonical_vector_index_with_control(table.clone(), field.clone(), 2, &control)
                    .err()
                    .unwrap();
                assert!(
                    if cancel {
                        matches!(error, uqa_storage::StorageBackendError::Cancelled(_))
                    } else {
                        matches!(error, uqa_storage::StorageBackendError::Memory(_))
                    },
                    "{mode}, spare {spare}"
                );
                assert_eq!(control.memory().used(), before, "{mode}, spare {spare}");
                drop((block, table, field));
                assert_eq!(control.memory().used(), 0, "{mode}, spare {spare}");
            }
        }
    }
}
