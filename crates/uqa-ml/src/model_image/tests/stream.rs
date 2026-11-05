//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;

fn drain(reader: &mut ImageReader<'_>, window: usize) -> Vec<u8> {
    let mut result = Vec::new();
    let mut bytes = vec![0xa5; window];
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    loop {
        let progress = reader.prepare(1).unwrap();
        assert!(progress.positions <= 1);
        if progress.ready {
            break;
        }
        assert_eq!(progress.positions, 1);
    }
    loop {
        bytes.fill(0xa5);
        let count = reader.read(&mut bytes).unwrap();
        assert!(count <= window.min(16 * 1024));
        assert!(bytes[count..].iter().all(|&byte| byte == 0xa5));
        if count == 0 {
            break;
        }
        result.extend_from_slice(&bytes[..count]);
    }
    result
}

#[test]
fn resumable_model_images_preserve_all_layers_and_wire_boundaries() {
    for mut source in fixtures::models() {
        for gating in [GatingSpec::None, GatingSpec::ReLU, GatingSpec::Swish] {
            source.gating = gating;
            let expected = encode(&source);
            for window in [1, 7, 4095, 32 * 1024] {
                let control = StorageReadControl::with_limit(64 * 1024);
                let mut reader = model_image_reader(&source, &control).unwrap();
                let bytes = drain(&mut reader, window);
                assert_eq!(bytes, expected);
                assert_eq!(control.memory().used(), 0);
                let restored = read_model_image(&mut bytes.as_slice(), &control).unwrap();
                assert_eq!(*restored, source);
            }
        }
    }
}

fn large() -> DeepModel {
    model(vec![
        DeepLayerSpec::Input {
            dimensions: 1 << 19,
        },
        DeepLayerSpec::Dense {
            weights: vec![f64::from_bits(1); 1 << 19],
            bias: vec![-0.0],
            output_channels: 1,
            input_channels: 1 << 19,
        },
    ])
}

#[test]
fn resumable_model_validation_and_output_bound_work_and_exact_scratch_admission() {
    let source = large();
    let expected = encode(&source);
    let probe = StorageReadControl::with_limit(64 * 1024);
    let cursor = model_image_reader(&source, &probe).unwrap();
    let needed = probe.memory().used();
    drop(cursor);
    assert_eq!(probe.memory().used(), 0);
    let denied = StorageReadControl::with_limit(needed - 1);
    assert!(matches!(
        model_image_reader(&source, &denied),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(denied.memory().used(), 0);
    let control = StorageReadControl::with_limit(needed);
    let mut reader = model_image_reader(&source, &control).unwrap();
    let mut positions = 0;
    let mut output = vec![0; 32 * 1024];
    let mut offset = 0;
    let allocation = allocation_counter::measure(|| {
        loop {
            let progress = reader.prepare(1).unwrap();
            assert_eq!(progress.positions, 1);
            positions += 1;
            if progress.ready {
                break;
            }
        }
        assert_eq!(reader.prepare(1).unwrap().positions, 0);
        loop {
            let count = reader.read(&mut output).unwrap();
            assert!(count <= 16 * 1024);
            if count == 0 {
                break;
            }
            assert_eq!(&output[..count], &expected[offset..offset + count]);
            offset += count;
        }
    });
    assert_eq!(positions, (1 << 19) / 512 + 4);
    assert_eq!(offset, expected.len());
    assert_eq!(allocation.count_total, 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn resumable_model_validation_preserves_error_order_and_absolute_positions() {
    let mut source = model(vec![
        DeepLayerSpec::Input { dimensions: 1025 },
        DeepLayerSpec::Dense {
            weights: vec![1.0; 1025],
            bias: vec![0.0],
            output_channels: 1,
            input_channels: 1025,
        },
    ]);
    let DeepLayerSpec::Dense { weights, .. } = &mut source.layers[1] else {
        unreachable!()
    };
    weights[1024] = f64::NAN;
    let diagnostic = "deep-fusion layer 1: weights[1024] must be finite, got NaN";
    assert!(source
        .validate()
        .unwrap_err()
        .to_string()
        .contains(diagnostic));
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut reader = model_image_reader(&source, &control).unwrap();
    let mut positions = 0;
    loop {
        match reader.prepare(1) {
            Ok(progress) => {
                assert!(!progress.ready);
                positions += progress.positions;
            }
            Err(error) => {
                assert!(error.to_string().contains(diagnostic));
                break;
            }
        }
    }
    assert_eq!(positions, 4);
    assert_eq!(control.memory().used(), 0);
    let mut untouched = [0xa5; 16];
    assert!(reader.read(&mut untouched).is_err());
    assert_eq!(untouched, [0xa5; 16]);
    assert!(reader.prepare(1).is_err());
    let mut bytes = Vec::new();
    assert!(write_model_image(&source, &mut bytes, &control).is_err());
    assert!(bytes.is_empty());
    drop(reader);
    let DeepLayerSpec::Dense { bias, .. } = &mut source.layers[1] else {
        unreachable!()
    };
    bias.clear();
    assert!(source
        .validate()
        .unwrap_err()
        .to_string()
        .contains("bias has length 0, expected 1"));
    for (weights, expected) in [
        (
            vec![-1.0, f64::NAN],
            "hop_weights[1] must be finite, got NaN",
        ),
        (
            vec![f64::MAX, f64::MAX],
            "hop_weights must be non-negative with a finite positive sum",
        ),
        (
            vec![-0.0, 0.0],
            "hop_weights must be non-negative with a finite positive sum",
        ),
    ] {
        let source = model(vec![
            DeepLayerSpec::Input { dimensions: 1 },
            DeepLayerSpec::Conv {
                edge_label: "edge".into(),
                hop_weights: weights,
                direction: crate::DirectionSpec::Both,
            },
        ]);
        let mut output = Vec::new();
        assert!(write_model_image(&source, &mut output, &control)
            .unwrap_err()
            .to_string()
            .contains(expected));
        assert!(source
            .validate()
            .unwrap_err()
            .to_string()
            .contains(expected));
        assert!(output.is_empty());
    }
}

#[test]
fn owned_model_readers_preserve_selected_source_after_replacement_and_thread_transfer() {
    let mut source = Arc::new(small());
    let expected = encode(&source);
    let capture = StorageReadControl::with_limit(1 << 20);
    let selected = source.capture_image_with_control(&capture).unwrap();
    let original = Arc::downgrade(&source);
    Arc::make_mut(&mut source).alpha = 0.75;
    drop(source);
    let output = StorageReadControl::with_limit(64 * 1024);
    let mut reader = selected.into_image_reader(&output).unwrap();
    assert!(original.upgrade().is_some());
    assert_eq!(
        std::thread::spawn(move || drain(&mut reader, 3))
            .join()
            .unwrap(),
        expected
    );
    assert!(original.upgrade().is_none());
    assert_eq!(capture.memory().used(), 0);
    assert_eq!(output.memory().used(), 0);
}

#[test]
fn owned_model_readers_retire_original_and_invoking_cancellation_during_preparation_and_output() {
    for original in [true, false] {
        for phase in [0, 1, 2, 3] {
            let source = Arc::new(large());
            let capture = StorageReadControl::with_limit(8 << 20);
            let selected = source.capture_image_with_control(&capture).unwrap();
            let weak = Arc::downgrade(&source);
            drop(source);
            let invoking = StorageReadControl::with_limit(64 * 1024);
            let mut reader = selected.into_image_reader(&invoking).unwrap();
            if phase > 0 {
                assert!(!reader.prepare(1).unwrap().ready);
            }
            if phase > 1 {
                while !reader.prepare(17).unwrap().ready {}
                assert_eq!(reader.read(&mut [0; 7]).unwrap(), 7);
            }
            if original {
                capture.cancellation().cancel();
            } else {
                invoking.cancellation().cancel();
            }
            let result = match phase {
                0 | 1 => reader.prepare(1).map(|_| 0),
                2 => reader.read(&mut [0; 7]),
                _ => reader.read(&mut []),
            };
            assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
            assert!(reader.prepare(1).is_err());
            assert!(reader.read(&mut [0; 7]).is_err());
            assert!(weak.upgrade().is_none());
            assert_eq!(capture.memory().used(), 0);
            assert_eq!(invoking.memory().used(), 0);
        }
    }
}
