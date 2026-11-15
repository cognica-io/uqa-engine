//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{DeepLayerSpec, GatingSpec};
use std::collections::BTreeMap;
use uqa_core::memory::BudgetedVec;
use uqa_storage::key_value::{KeyValueCatalog, MemoryKeyValueStore};

type Rows = BudgetedVec<(Budgeted<String>, Budgeted<DeepModel>)>;

fn rows(width: usize, control: &StorageReadControl) -> (Rows, BTreeMap<String, (usize, f64)>) {
    let mut rows = BudgetedVec::new(control.memory());
    let mut expected = BTreeMap::new();
    for (index, key) in (0..8).rev().chain([3, 5, 0]).enumerate() {
        let name = format!("{}-{key:02}", "a".repeat(width));
        let name_memory = control.memory().reserve(name.capacity()).unwrap();
        let model_memory = control
            .memory()
            .reserve(size_of::<DeepModel>() + size_of::<DeepLayerSpec>() + 256 * size_of::<f64>())
            .unwrap();
        let value = index as f64;
        let weights = vec![value; 256];
        expected.insert(name.clone(), (weights.as_ptr() as usize, value));
        let model = DeepModel {
            layers: vec![DeepLayerSpec::Embed { embedding: weights }],
            alpha: value,
            gating: GatingSpec::None,
        };
        rows.push((
            Budgeted::new(name, name_memory),
            Budgeted::new(model, model_memory),
        ))
        .unwrap();
    }
    (rows, expected)
}

fn adoption(
    rows: Rows,
    control: &StorageReadControl,
    mutations: &MemoryBudget,
) -> ImageDecoder<ModelCatalog, StorageBackendError> {
    let mutations = mutations.clone();
    ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move {
            let mut work = reader.input();
            let mut catalog = ModelCatalog::new(work.memory);
            let (rows, _memory) = rows.into_parts();
            for (name, model) in rows {
                catalog = catalog.insert_restored_from(name, model, &mut work).await?;
            }
            Ok(catalog.into_mutation_budget(&mutations))
        },
    )
    .unwrap()
}

#[test]
fn stored_model_publication_resumes_names_and_retains_exact_replacement_weights() {
    let mut observed = Vec::new();
    for width in [0, 65536] {
        let control = StorageReadControl::with_limit(8 << 20);
        let mutations = MemoryBudget::new(8 << 20);
        let (rows, expected) = rows(width, &control);
        let mut input = adoption(rows, &control, &mutations);
        let mut polls = 0;
        let mut catalog = loop {
            polls += 1;
            assert!(polls < 65536);
            if let Some(catalog) = input
                .advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 },
                )
                .unwrap()
            {
                break catalog;
            }
        };
        observed.push(polls);
        assert_eq!(catalog.values.len(), expected.len());
        for (name, (pointer, value)) in &expected {
            let handle = catalog.get(name).unwrap();
            assert_eq!(handle.alpha, *value);
            let DeepLayerSpec::Embed { embedding } = &handle.layers[0] else {
                panic!("stored embedding");
            };
            assert_eq!(embedding.as_ptr() as usize, *pointer);
            assert_eq!(embedding, &vec![*value; 256]);
        }
        let name = expected.keys().next().unwrap();
        let retained = catalog.get(name).unwrap().clone();
        let identity = retained.identity();
        let occupied = control
            .memory()
            .reserve(control.memory().available())
            .unwrap();
        control.cancellation().cancel();
        catalog
            .insert(
                name.clone(),
                Arc::new(DeepModel {
                    layers: vec![],
                    alpha: -1.0,
                    gating: GatingSpec::None,
                }),
            )
            .unwrap();
        assert_eq!(retained.alpha, expected[name].1);
        drop((catalog, input));
        assert!(identity.is_live());
        assert!(control.memory().used() > occupied.bytes());
        drop(retained);
        assert!(!identity.is_live());
        assert_eq!(control.memory().used(), occupied.bytes());
        drop(occupied);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(mutations.used(), 0);
    }
    assert!(observed[1] > observed[0] + 128, "{observed:?}");
}

#[test]
fn stored_model_publication_retires_every_terminal_prefix() {
    for prefix in [1, 16, 128] {
        for terminal in ["cancel", "memory", "zero", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(8 << 20);
            let mutations = MemoryBudget::new(8 << 20);
            let (rows, _) = rows(65536, &control);
            let mut input = adoption(rows, &control, &mutations);
            for _ in 0..prefix {
                assert!(input
                    .advance(
                        &mut std::io::empty(),
                        ImageDecodeWindow { bytes: 1, steps: 1 }
                    )
                    .unwrap()
                    .is_none());
            }
            let occupied = (terminal == "memory").then(|| {
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap()
            });
            if terminal == "cancel" {
                control.cancellation().cancel();
            }
            if terminal == "unwind" {
                assert!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _input = input;
                        panic!("retire private model input");
                    }))
                    .is_err()
                );
            } else {
                if terminal != "drop" {
                    let window = ImageDecodeWindow {
                        bytes: 1,
                        steps: usize::from(terminal != "zero"),
                    };
                    let mut rejected = false;
                    for _ in 0..65536 {
                        match input.advance(&mut std::io::empty(), window) {
                            Err(_) => {
                                rejected = true;
                                break;
                            }
                            Ok(None) => (),
                            Ok(Some(_)) => panic!("terminal model input completed"),
                        }
                    }
                    assert!(rejected);
                    assert!(input.advance(&mut std::io::empty(), window).is_err());
                }
                drop(input);
            }
            drop(occupied);
            assert_eq!(control.memory().used(), 0, "{prefix}/{terminal}");
            assert_eq!(mutations.used(), 0);
        }
    }
}

#[test]
fn stored_model_catalog_input_requires_versioned_physical_ownership() {
    let catalog: Arc<dyn CatalogFacade> =
        Arc::new(KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new())));
    catalog.save_model("model", r#"{"layers":[]}"#).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let limits = CatalogInputLimits {
        maximum_key_bytes: 1 << 20,
        maximum_value_bytes: 1 << 20,
    };
    let mut input =
        ModelCatalogInput::new(catalog.clone(), limits, control.memory(), &control).unwrap();
    let error = loop {
        match input.advance(1) {
            Err(error) => break error,
            Ok(None) => (),
            Ok(Some(_)) => panic!("unversioned model source was accepted"),
        }
    };
    assert!(
        error
            .to_string()
            .contains("requires a versioned read owner"),
        "{error}"
    );
    assert!(input.advance(1).is_err());
    assert_eq!(control.memory().used(), 0);
    assert_eq!(Arc::strong_count(&catalog), 1);
}
