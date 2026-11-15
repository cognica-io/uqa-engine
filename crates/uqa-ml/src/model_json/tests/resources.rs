//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn text() -> String {
    format!(
        r#"{{"layers":[{{"kind":"embed","embedding":[{}]}}],"alpha":-0.0}}"#,
        "0.25,".repeat(8192) + "-0.0"
    )
}

#[test]
fn stored_model_json_retains_admitted_weights_after_input_and_cancellation_retire() {
    let text = text();
    let memory = MemoryBudget::new(8 << 20);
    let cancellation = CancellationToken::new();
    let mut input = decoder(&text, &memory, &cancellation);
    let model = finish(&mut input, 1).unwrap();
    drop((input, text));
    cancellation.cancel();
    let DeepLayerSpec::Embed { embedding } = &model.layers[0] else {
        panic!("stored embedding");
    };
    assert_eq!(embedding.len(), 8193);
    assert_eq!(embedding[0], 0.25);
    assert_eq!(embedding[8192].to_bits(), (-0.0_f64).to_bits());
    assert_eq!(model.alpha.to_bits(), (-0.0_f64).to_bits());
    assert!(memory.used() >= size_of::<DeepModel>() + 8193 * size_of::<f64>());
    assert_eq!(memory.used(), model.reserved_bytes());
    drop(model);
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_model_json_retires_every_selected_construction_prefix() {
    let text = text();
    let mut memory_failures = 0;
    let mut memory_completions = 0;
    for prefix in [1, 8, 64, 512, 4096, 32768] {
        for terminal in ["cancel", "memory", "zero", "drop", "unwind"] {
            let memory = MemoryBudget::new(8 << 20);
            let cancellation = CancellationToken::new();
            let mut input = decoder(&text, &memory, &cancellation);
            for _ in 0..prefix {
                assert!(input
                    .advance(
                        &mut std::io::empty(),
                        ImageDecodeWindow { bytes: 1, steps: 1 }
                    )
                    .unwrap()
                    .is_none());
            }
            let occupied =
                (terminal == "memory").then(|| memory.reserve(memory.available()).unwrap());
            if terminal == "cancel" {
                cancellation.cancel();
            }
            let mut completed = None;
            if terminal == "unwind" {
                assert!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _input = input;
                        panic!("retire stored model construction");
                    }))
                    .is_err()
                );
            } else {
                if terminal != "drop" {
                    let result = finish(&mut input, usize::from(terminal != "zero"));
                    match (terminal, result) {
                        ("cancel", Err(JsonInputError::Cancelled(_)))
                        | ("zero", Err(JsonInputError::Image(_))) => (),
                        ("memory", Err(JsonInputError::Memory(_))) => memory_failures += 1,
                        ("memory", Ok(model)) => {
                            memory_completions += 1;
                            completed = Some(model);
                        }
                        (_, result) => panic!(
                            "{prefix}/{terminal}: {:?}",
                            result.as_ref().map(Budgeted::reserved_bytes)
                        ),
                    }
                    assert!(input
                        .advance(
                            &mut std::io::empty(),
                            ImageDecodeWindow { bytes: 1, steps: 1 }
                        )
                        .is_err());
                }
                drop(input);
            }
            if let Some(model) = completed {
                assert!(model.budget().shares_allowance(&memory));
                assert_eq!(
                    memory.used(),
                    occupied.as_ref().unwrap().bytes() + model.reserved_bytes()
                );
                let DeepLayerSpec::Embed { embedding } = &model.layers[0] else {
                    panic!("completed stored embedding");
                };
                assert_eq!(embedding.len(), 8193);
                assert_eq!(embedding[8192].to_bits(), (-0.0_f64).to_bits());
                drop(model);
            }
            drop(occupied);
            assert_eq!(memory.used(), 0, "{prefix}/{terminal}");
        }
    }
    assert!(memory_failures > 0);
    assert!(memory_completions > 0);
}
