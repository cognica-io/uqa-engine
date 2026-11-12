//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn field(control: &StorageReadControl) -> StorageName {
    let mut value = BudgetedString::new(control.memory());
    value.push_str("embedding").unwrap();
    let (value, memory) = value.into_parts();
    StorageName::from_admitted(Budgeted::new(value, memory)).unwrap()
}

#[test]
fn controlled_vector_handles_keep_original_names_after_input_retirement() {
    let backend = super::super::super::KeyValueStorageBackend::new(store());
    let control = StorageReadControl::with_limit(1 << 20);
    let table = name(&control);
    let field = field(&control);
    let before = control.memory().used();
    let mut index = None;
    let allocations = allocation_counter::measure(|| {
        index = Some(
            backend
                .canonical_vector_index_with_control(table.clone(), field.clone(), 2, &control)
                .unwrap(),
        );
    });
    assert_eq!(
        allocations.count_total, 1,
        "only the concrete handle is allocated"
    );
    assert_eq!(
        control.memory().used() - before,
        size_of::<super::super::super::KeyValueVectorIndex>()
    );
    let mut index = index.unwrap();
    drop((table, field));
    control.cancellation().cancel();
    index.add(1, vec![1.0, 0.0]).unwrap();
    index.add(2, vec![0.0, 1.0]).unwrap();
    let hits = index.search_knn(&[1.0, 0.0], 2).unwrap();
    assert_eq!(
        hits.entries()
            .iter()
            .map(|entry| entry.doc_id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    index.delete(2).unwrap();
    assert!(index.contains_document(1).unwrap());
    assert!(!index.contains_document(2).unwrap());
    assert!(control.memory().used() > before);
    drop(index);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn controlled_vector_handle_rejection_retires_original_names_and_headers() {
    let backend = super::super::super::KeyValueStorageBackend::new(store());
    let control = StorageReadControl::with_limit(1 << 20);
    let other = StorageReadControl::with_limit(1 << 20);
    assert!(backend
        .canonical_vector_index_with_control(name(&control), field(&other), 2, &control)
        .is_err());
    assert_eq!((control.memory().used(), other.memory().used()), (0, 0));
    for cancel in [false, true] {
        let control = StorageReadControl::with_limit(1 << 20);
        let table = name(&control);
        let field = field(&control);
        let block = control
            .memory()
            .reserve(control.memory().available())
            .unwrap();
        if cancel {
            control.cancellation().cancel();
        }
        let error = backend
            .canonical_vector_index_with_control(table.clone(), field.clone(), 2, &control)
            .err()
            .unwrap();
        assert!(if cancel {
            matches!(error, StorageBackendError::Cancelled(_))
        } else {
            matches!(error, StorageBackendError::Memory(_))
        });
        assert_eq!(control.memory().used(), control.memory().limit());
        drop((block, table, field));
        assert_eq!(control.memory().used(), 0);
    }
}
