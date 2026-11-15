//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::key_value::{KeyValueCatalog, MemoryKeyValueStore};
use std::collections::BTreeMap;
use uqa_core::memory::BudgetedVec;

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 1 << 20,
    maximum_value_bytes: 1 << 20,
};

fn source() -> Arc<dyn CatalogFacade> {
    Arc::new(KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new())))
}

fn finish(
    input: &mut ScoringParameterCatalogInput,
    steps: usize,
) -> StorageBackendResult<(ScoringParameters, usize)> {
    for polls in 1..1_000_000 {
        if let Some(value) = input.advance(steps)? {
            return Ok((value, polls));
        }
    }
    panic!("scoring catalog input stopped progressing")
}

type Expected = BTreeMap<String, (usize, usize, String)>;
type Rows = BudgetedVec<(String, Budgeted<String>)>;

fn admitted_rows(prefix: &str, control: &StorageReadControl) -> (Rows, Expected) {
    let mut rows = Rows::new(control.memory());
    let mut expected = Expected::new();
    for index in (0..8).rev().chain([3, 5, 0]) {
        let name = format!("{prefix}-{index:02}");
        let encoded = format!("opaque\0{}:{{\"x\":1,\"x\":2}}", rows.len());
        let memory = control
            .memory()
            .reserve(name.capacity() + encoded.capacity())
            .unwrap();
        expected.insert(
            name.clone(),
            (
                name.as_ptr() as usize,
                encoded.as_ptr() as usize,
                encoded.clone(),
            ),
        );
        rows.push((name, Budgeted::new(encoded, memory))).unwrap();
    }
    (rows, expected)
}

fn adoption(
    rows: Rows,
    control: &StorageReadControl,
) -> ImageDecoder<ScoringParameters, StorageBackendError> {
    ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move {
            let mut input = reader.input();
            let mut catalog = ScoringParameters::new(input.memory);
            let (rows, _row_memory) = rows.into_parts();
            for (name, value) in rows {
                catalog = catalog
                    .insert_restored_from(name, value, &mut input)
                    .await?;
            }
            Ok(catalog)
        },
    )
    .unwrap()
}

#[test]
fn scoring_publication_resumes_long_comparisons_and_transfers_last_definition_allocations() {
    let mut observed = Vec::new();
    for width in [0, 65536] {
        let control = StorageReadControl::with_limit(8 << 20);
        let (rows, expected) = admitted_rows(&"a".repeat(width), &control);
        let mut input = adoption(rows, &control);
        let mut polls = 0;
        let restored = loop {
            polls += 1;
            assert!(polls < 65536);
            let mut outcome = None;
            let allocation = allocation_counter::measure(|| {
                outcome = Some(input.advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 },
                ));
            });
            assert!(allocation.bytes_total < 16384, "{allocation:?}");
            if let Some(value) = outcome.unwrap().unwrap() {
                break value;
            }
        };
        observed.push(polls);
        assert_eq!(restored.len(), expected.len());
        for (name, value) in restored.iter() {
            let (key_pointer, value_pointer, expected) = &expected[name];
            assert_eq!(value, expected);
            assert_eq!(name.as_ptr() as usize, *key_pointer);
            assert_eq!(value.as_ptr() as usize, *value_pointer);
        }
        drop((restored, input));
        assert_eq!(control.memory().used(), 0);
    }
    assert!(observed[1] > observed[0] + 128, "{observed:?}");
}

#[test]
fn scoring_publication_retires_private_rows_and_paths_on_every_terminal_prefix() {
    for prefix in [1, 16, 128] {
        for terminal in ["cancel", "memory", "zero", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(8 << 20);
            let (rows, _) = admitted_rows(&"a".repeat(65536), &control);
            let mut input = adoption(rows, &control);
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
                        panic!("retire private scoring publication");
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
                            Ok(Some(_)) => panic!("terminal scoring publication completed"),
                        }
                    }
                    assert!(rejected);
                    assert!(input.advance(&mut std::io::empty(), window).is_err());
                }
                drop(input);
            }
            drop(occupied);
            assert_eq!(control.memory().used(), 0, "{prefix}/{terminal}");
        }
    }
}

#[test]
fn stored_scoring_input_rejects_unversioned_sources_and_retires_the_selected_provider() {
    let catalog = source();
    catalog
        .save_scoring_params("signal", &"x".repeat(16384))
        .unwrap();
    for limits in [
        CatalogInputLimits {
            maximum_key_bytes: 0,
            ..LIMITS
        },
        CatalogInputLimits {
            maximum_value_bytes: 0,
            ..LIMITS
        },
        LIMITS,
    ] {
        let control = StorageReadControl::with_limit(1 << 20);
        match ScoringParameterCatalogInput::new(catalog.clone(), limits, control.memory(), &control)
        {
            Err(_) => assert!(limits.maximum_key_bytes == 0 || limits.maximum_value_bytes == 0),
            Ok(mut input) => {
                let error = finish(&mut input, 1)
                    .err()
                    .expect("unversioned source is rejected");
                assert!(
                    error
                        .to_string()
                        .contains("requires a versioned read owner"),
                    "{error}"
                );
                assert!(input.advance(1).is_err());
            }
        }
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(Arc::strong_count(&catalog), 1);
    assert_eq!(catalog.load_all_scoring_params().unwrap().len(), 1);
}
