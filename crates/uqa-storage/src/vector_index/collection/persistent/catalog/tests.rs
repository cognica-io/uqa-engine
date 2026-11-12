//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    key_value::{KeyValueStorageBackend, MemoryKeyValueStore},
    vector_index::{VectorIndexOpenMode, VectorIndexSpec},
};
use uqa_core::image_io::ImageDecodeWindow;

fn backend() -> Arc<dyn PersistentStorageBackend> {
    Arc::new(KeyValueStorageBackend::new(Arc::new(
        MemoryKeyValueStore::new(),
    )))
}

fn fields(control: &StorageReadControl) -> Budgeted<Vec<VectorFieldSchema>> {
    let wire = serde_json::to_vec(&[
        VectorFieldSchema {
            field: "z".into(),
            dimensions: 2,
        },
        VectorFieldSchema {
            field: "경계".repeat(1024),
            dimensions: 2,
        },
        VectorFieldSchema {
            field: "a".into(),
            dimensions: 4,
        },
        VectorFieldSchema {
            field: "z".into(),
            dimensions: 3,
        },
    ])
    .unwrap();
    crate::catalog::input::table::vector_fields(&wire, control).unwrap()
}

fn table(control: &StorageReadControl) -> StorageName {
    let mut name = uqa_core::memory::BudgetedString::new(control.memory());
    name.push_str("public.notes").unwrap();
    let (name, memory) = name.into_parts();
    StorageName::from_admitted(Budgeted::new(name, memory)).unwrap()
}

fn input(control: &StorageReadControl) -> ImageDecoder<VectorIndexes, StorageBackendError> {
    PersistentVectorIndexesBuilder::catalog_input(
        backend(),
        table(control),
        fields(control),
        control,
    )
    .unwrap()
}

fn advance(
    input: &mut ImageDecoder<VectorIndexes, StorageBackendError>,
) -> StorageBackendResult<Option<VectorIndexes>> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn persistent_vector_catalog_adopts_original_keys_and_preserves_repeated_field_selection() {
    let control = StorageReadControl::with_limit(1 << 20);
    let backend = backend();
    let fields = fields(&control);
    let pointers = [
        fields[0].field.as_ptr(),
        fields[2].field.as_ptr(),
        fields[1].field.as_ptr(),
    ];
    let expected: std::collections::BTreeMap<_, _> = fields
        .iter()
        .map(|field| {
            let index = backend
                .vector_index(
                    "public.notes",
                    &field.field,
                    field.dimensions,
                    VectorIndexSpec::BruteForce,
                    VectorIndexOpenMode::Restore,
                )
                .unwrap();
            (field.field.clone(), index.dimensions())
        })
        .collect();
    let mut input =
        PersistentVectorIndexesBuilder::catalog_input(backend, table(&control), fields, &control)
            .unwrap();
    let mut calls = 0;
    let mut indexes = loop {
        calls += 1;
        if let Some(value) = advance(&mut input).unwrap() {
            break value;
        }
        assert!(calls < 100);
    };
    drop(input);
    assert!(
        calls > 4,
        "field names and visits must retain bounded progress"
    );
    let actual: std::collections::BTreeMap<_, _> = indexes
        .iter()
        .map(|(field, index)| (field.clone(), index.dimensions()))
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(
        indexes.keys().find(|field| *field == "z").unwrap().as_ptr(),
        pointers[0]
    );
    assert_eq!(
        indexes.keys().find(|field| *field == "a").unwrap().as_ptr(),
        pointers[1]
    );
    assert_eq!(indexes.keys().last().unwrap().as_ptr(), pointers[2]);
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    indexes
        .live_mut()
        .unwrap()
        .get_mut("z")
        .unwrap()
        .add(1, vec![1.0, 0.0, 0.0])
        .unwrap();
    assert_eq!(
        indexes["z"]
            .search_knn(&[1.0, 0.0, 0.0], 1)
            .unwrap()
            .entries()[0]
            .doc_id,
        1
    );
    drop(indexes);
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn persistent_vector_catalog_retires_every_pending_prefix_on_drop_cancel_and_denied_admission() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut complete = input(&control);
    let mut pending = 0;
    loop {
        if let Some(indexes) = advance(&mut complete).unwrap() {
            drop(indexes);
            break;
        }
        pending += 1;
        assert!(pending < 100);
    }
    drop(complete);
    assert_eq!(control.memory().used(), 0);
    for boundary in 0..=pending {
        for cancel in [false, true] {
            let control = StorageReadControl::with_limit(1 << 20);
            let mut current = input(&control);
            for _ in 0..boundary {
                assert!(advance(&mut current).unwrap().is_none());
            }
            if cancel {
                control.cancellation().cancel();
                assert!(matches!(
                    advance(&mut current),
                    Err(StorageBackendError::Cancelled(_))
                ));
            }
            drop(current);
            assert_eq!(control.memory().used(), 0, "boundary {boundary}");
        }
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let mut current = input(&control);
    assert!(advance(&mut current).unwrap().is_none());
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    let error = loop {
        match advance(&mut current) {
            Err(error) => break error,
            Ok(None) => (),
            Ok(Some(_)) => panic!("unfunded catalog reached publication"),
        }
    };
    assert!(matches!(error, StorageBackendError::Memory(_)));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop((current, occupied));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn persistent_vector_catalog_rejects_foreign_and_short_original_field_owners() {
    for invalid in 0..3 {
        let control = StorageReadControl::with_limit(1 << 20);
        let foreign = StorageReadControl::with_limit(1 << 20);
        let values = if invalid == 0 {
            fields(&foreign)
        } else {
            let values = vec![VectorFieldSchema {
                field: "unfunded".into(),
                dimensions: 2,
            }];
            let bytes = if invalid == 1 {
                0
            } else {
                values.capacity() * size_of::<VectorFieldSchema>()
            };
            Budgeted::new(values, control.memory().reserve(bytes).unwrap())
        };
        let result = PersistentVectorIndexesBuilder::catalog_input(
            backend(),
            table(&control),
            values,
            &control,
        );
        let error = match result {
            Err(error) => error,
            Ok(mut current) => loop {
                match advance(&mut current) {
                    Err(error) => break error,
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("incomplete field allowance reached publication"),
                }
            },
        };
        assert!(matches!(error, StorageBackendError::Other(_)));
        assert_eq!((control.memory().used(), foreign.memory().used()), (0, 0));
    }
}
