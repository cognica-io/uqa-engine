//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_ml::{DeepLayerSpec, DeepModel, GatingSpec};

fn model_source() -> Engine {
    let source = Engine::new();
    source
        .save_model(
            "saved_model\0한글",
            &DeepModel {
                layers: vec![DeepLayerSpec::Embed {
                    embedding: vec![-0.0; 8193],
                }],
                alpha: 0.25,
                gating: GatingSpec::ReLU,
            },
        )
        .unwrap();
    source
}

#[test]
fn repository_model_input_obeys_current_windows_after_source_retirement() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = model_source();
    let expected = image(&source);
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let mut calls = 0;
    let mut resource_calls = 0;
    let mut allowed = 0;
    let mut consumed = 0;
    let state = loop {
        let maximum = if calls % 2 == 0 { 1 } else { 17 };
        calls += 1;
        if let Some(state) = input.advance(maximum).unwrap() {
            break state;
        }
        if let Some(unit) = input.image.as_ref().unwrap().current_unit() {
            if matches!(
                unit.address,
                DecodedMemoryImageAddress::Resource { kind: 2, .. }
            ) {
                resource_calls += 1;
                allowed += maximum as u64;
                consumed = unit.decoded_bytes;
            }
        }
    };
    assert!(resource_calls > 1, "model completed in one call");
    assert!(
        consumed <= allowed,
        "{consumed} bytes consumed with {allowed} bytes of call allowance"
    );
    drop(input);
    let restored = state
        .into_private_engine(MemoryBudget::new(32 << 20), &control)
        .unwrap();
    repository
        .remove_backup(root.backup(), &StorageReadControl::with_limit(1 << 20))
        .unwrap();
    drop(repository);
    control.cancellation().cancel();
    assert_eq!(image(&restored), expected);
    let mut model = restored.load_model("saved_model\0한글").unwrap().unwrap();
    let DeepLayerSpec::Embed { embedding } = &mut model.layers[0] else {
        panic!("stored embedding");
    };
    assert_eq!(embedding[8192].to_bits(), (-0.0_f64).to_bits());
    embedding[8192] = f64::from_bits(1);
    restored.save_model("saved_model\0한글", &model).unwrap();
    assert_ne!(image(&restored), expected);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn repository_model_interruption_retires_the_private_destination() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = model_source();
    let root = publish(&source, &repository);
    drop(source);
    for mode in 0..4 {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&repository, root, &control);
        let mut calls = 0;
        loop {
            assert!(input.advance(1).unwrap().is_none());
            if matches!(
                input
                    .image
                    .as_ref()
                    .unwrap()
                    .current_unit()
                    .map(|unit| unit.address),
                Some(DecodedMemoryImageAddress::Resource { kind: 2, .. })
            ) {
                calls += 1;
                if calls == 30 {
                    break;
                }
            }
        }
        let held = if mode == 1 {
            Some(
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap(),
            )
        } else {
            None
        };
        if mode == 0 {
            control.cancellation().cancel();
        }
        if mode != 2 {
            while let Ok(state) = input.advance(usize::from(mode != 3)) {
                assert!(state.is_none());
            }
            assert!(input.candidate.is_none() && input.image.is_none());
            assert!(matches!(input.advance(1), Err(BackupError::Finished)));
        }
        drop((input, held));
        assert_eq!(control.memory().used(), 0, "interruption {mode}");
    }
}
