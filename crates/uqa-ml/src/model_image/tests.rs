//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{DeepLayerSpec, GatingSpec};
use std::io::Cursor;

mod capture;
mod fixtures;

fn model(layers: Vec<DeepLayerSpec>) -> DeepModel {
    DeepModel {
        layers,
        alpha: -0.0,
        gating: GatingSpec::ReLU,
    }
}

fn encode(model: &DeepModel) -> Vec<u8> {
    let mut bytes = Vec::new();
    assert_eq!(
        write_model_image(model, &mut bytes, &StorageReadControl::with_limit(0)).unwrap(),
        bytes.len() as u64
    );
    bytes
}

fn small() -> DeepModel {
    model(vec![
        DeepLayerSpec::Input { dimensions: 1 },
        DeepLayerSpec::Dense {
            weights: vec![-0.0],
            bias: vec![f64::from_bits(1)],
            output_channels: 1,
            input_channels: 1,
        },
    ])
}

#[test]
fn model_image_matches_literal_tags_counts_and_ieee_bits() {
    let source = small();
    let mut expected = b"UQAMOD01".to_vec();
    expected.extend(0x8000_0000_0000_0000_u64.to_le_bytes());
    expected.push(1);
    expected.extend(2_u64.to_le_bytes());
    expected.push(0);
    expected.extend(1_u64.to_le_bytes());
    expected.push(2);
    expected.extend(1_u64.to_le_bytes());
    expected.extend(0x8000_0000_0000_0000_u64.to_le_bytes());
    expected.extend(1_u64.to_le_bytes());
    expected.extend(1_u64.to_le_bytes());
    expected.extend(1_u64.to_le_bytes());
    expected.extend(1_u64.to_le_bytes());
    assert_eq!(encode(&source), expected);
    let control = StorageReadControl::with_limit(1 << 20);
    let restored = read_model_image(&mut expected.as_slice(), &control).unwrap();
    assert_eq!(restored.alpha.to_bits(), source.alpha.to_bits());
    let DeepLayerSpec::Dense { weights, bias, .. } = &restored.layers[1] else {
        panic!("dense layer");
    };
    assert_eq!(weights[0].to_bits(), (-0.0_f64).to_bits());
    assert_eq!(bias[0].to_bits(), 1);
    assert_eq!(encode(&restored), expected);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn model_images_preserve_every_layer_variant_and_enum_after_source_loss() {
    for source in fixtures::models() {
        for gating in [GatingSpec::None, GatingSpec::ReLU, GatingSpec::Swish] {
            let source = DeepModel {
                gating,
                ..source.clone()
            };
            let bytes = encode(&source);
            let control = StorageReadControl::with_limit(1 << 20);
            let restored = read_model_image(&mut bytes.as_slice(), &control).unwrap();
            assert_eq!(*restored, source);
            drop(source);
            assert_eq!(encode(&restored), bytes);
            let restored = restored.into_shared().unwrap();
            let retained = restored.clone();
            drop(restored);
            assert!(control.memory().used() > 0);
            drop(retained);
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn model_image_restoration_preserves_cpu_scores_and_probabilities_without_training() {
    let source = model(vec![
        DeepLayerSpec::Input { dimensions: 2 },
        DeepLayerSpec::Dense {
            weights: vec![0.5, -0.75, 0.25, 0.5],
            bias: vec![0.125, -0.25],
            output_channels: 2,
            input_channels: 2,
        },
        DeepLayerSpec::Softmax,
    ]);
    let samples = [(41, vec![0.25, 0.75]), (7, vec![1.0, -0.5])];
    let expected = source.predict_features(&samples).unwrap();
    let bytes = encode(&source);
    drop(source);
    let control = StorageReadControl::with_limit(1 << 20);
    let restored = read_model_image(&mut bytes.as_slice(), &control).unwrap();
    assert_eq!(restored.predict_features(&samples).unwrap(), expected);
}

#[test]
fn model_image_rejects_incomplete_invalid_and_unfunded_input_without_retention() {
    let bytes = encode(&small());
    let control = StorageReadControl::with_limit(1 << 20);
    for end in 0..bytes.len() {
        assert!(
            read_model_image(&mut &bytes[..end], &control).is_err(),
            "prefix {end}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    let mut invalid_inputs = Vec::new();
    let mut trailing = bytes.clone();
    trailing.push(0);
    invalid_inputs.push(trailing);
    for (offset, value) in [(0, 0), (16, 255), (25, 255), (26, 0)] {
        let mut input = bytes.clone();
        input[offset] = value;
        invalid_inputs.push(input);
    }
    let mut nan = bytes.clone();
    nan[8..16].copy_from_slice(&f64::NAN.to_bits().to_le_bytes());
    invalid_inputs.push(nan);
    for offset in [17, 35] {
        let mut input = bytes.clone();
        input[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        invalid_inputs.push(input);
    }
    for input in invalid_inputs {
        assert!(read_model_image(&mut input.as_slice(), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let empty = StorageReadControl::with_limit(0);
    assert!(matches!(
        read_model_image(&mut bytes.as_slice(), &empty),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(empty.memory().used(), 0);
    let invalid_source = model(Vec::new());
    let mut output = Vec::new();
    assert!(write_model_image(&invalid_source, &mut output, &control).is_err());
    assert!(output.is_empty());
}

#[test]
fn model_image_large_weights_do_not_duplicate_validation_or_export_payloads() {
    struct Sink {
        bytes: usize,
        calls: usize,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            assert!(bytes.len() <= 4096);
            self.bytes += bytes.len();
            self.calls += 1;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let count = 1 << 18;
    let payload = count * size_of::<f64>();
    let source = model(vec![DeepLayerSpec::Embed {
        embedding: vec![0.25; count],
    }]);
    let mut sink = Sink { bytes: 0, calls: 0 };
    let control = StorageReadControl::with_limit(0);
    let export = allocation_counter::measure(|| {
        write_model_image(&source, &mut sink, &control).unwrap();
    });
    assert!(export.bytes_total < 1024, "{export:?}");
    assert!(sink.calls < count / 32);
    let bytes = encode(&source);
    assert_eq!(sink.bytes, bytes.len());
    let control = StorageReadControl::with_limit(payload * 2);
    let mut restored = None;
    let imported = allocation_counter::measure(|| {
        restored = Some(read_model_image(&mut bytes.as_slice(), &control).unwrap());
    });
    assert!(imported.bytes_total < payload as u64 + 4096, "{imported:?}");
    assert_eq!(**restored.as_ref().unwrap(), source);
    assert!(control.memory().used() >= payload);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
    let small = StorageReadControl::with_limit(64 << 10);
    let rejected = allocation_counter::measure(|| {
        assert!(matches!(
            read_model_image(&mut bytes.as_slice(), &small),
            Err(StorageBackendError::Memory(_))
        ));
    });
    assert!(rejected.bytes_total < 4096, "{rejected:?}");
    assert_eq!(small.memory().used(), 0);
}

#[test]
fn model_image_partial_io_polls_cancellation_and_retires_candidate_memory() {
    struct Output<'a> {
        control: &'a StorageReadControl,
        calls: usize,
    }
    impl Write for Output<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 5 {
                self.control.cancellation().cancel();
            }
            Ok(bytes.len().min(3))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    struct Input<'a> {
        source: Cursor<Vec<u8>>,
        control: &'a StorageReadControl,
        calls: usize,
    }
    impl Read for Input<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 20 {
                self.control.cancellation().cancel();
            }
            let length = bytes.len().min(3);
            self.source.read(&mut bytes[..length])
        }
    }
    let source = small();
    let control = StorageReadControl::with_limit(0);
    let mut output = Output {
        control: &control,
        calls: 0,
    };
    assert!(matches!(
        write_model_image(&source, &mut output, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(output.calls, 5);
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = Input {
        source: Cursor::new(encode(&source)),
        control: &control,
        calls: 0,
    };
    assert!(matches!(
        read_model_image(&mut input, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(input.calls, 20);
    assert_eq!(control.memory().used(), 0);
    assert!(source.validate().is_ok());
}

#[test]
fn model_image_rejects_invalid_label_and_enum_payloads_without_a_candidate() {
    use crate::{AggregationSpec, DirectionSpec, PoolKindSpec, PoolMethodSpec};
    let control = StorageReadControl::with_limit(1 << 20);
    let graph = model(vec![
        DeepLayerSpec::Input { dimensions: 1 },
        DeepLayerSpec::Propagate {
            edge_label: "x".into(),
            aggregation: AggregationSpec::Mean,
            direction: DirectionSpec::Out,
        },
    ]);
    let bytes = encode(&graph);
    for offset in [bytes.len() - 1, bytes.len() - 2, 43] {
        let mut corrupted = bytes.clone();
        corrupted[offset] = 255;
        assert!(read_model_image(&mut corrupted.as_slice(), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let mut variants = vec![
        (
            model(vec![
                DeepLayerSpec::Input { dimensions: 1 },
                DeepLayerSpec::GlobalPool {
                    method: PoolMethodSpec::Avg,
                },
            ]),
            1,
        ),
        (
            model(vec![
                DeepLayerSpec::Input { dimensions: 1 },
                DeepLayerSpec::Pool {
                    edge_label: String::new(),
                    pool_size: 1,
                    method: PoolKindSpec::Avg,
                    direction: DirectionSpec::Out,
                },
            ]),
            2,
        ),
    ];
    variants.extend(
        fixtures::models()
            .into_iter()
            .filter(|m| {
                matches!(
                    m.layers.last(),
                    Some(DeepLayerSpec::RNN { .. } | DeepLayerSpec::LSTM { .. })
                )
            })
            .map(|m| (m, 1)),
    );
    for (source, back) in variants {
        let mut bytes = encode(&source);
        let offset = bytes.len() - back;
        bytes[offset] = 255;
        assert!(read_model_image(&mut bytes.as_slice(), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn borrowed_model_validation_preserves_runtime_shape_and_numeric_diagnostics() {
    let mut candidates = fixtures::models();
    let mut invalid = Vec::new();
    for original in &candidates {
        let mut candidate = original.clone();
        match candidate.layers.last_mut().unwrap() {
            DeepLayerSpec::Input { dimensions } => *dimensions = 0,
            DeepLayerSpec::Embed { embedding } => embedding[0] = f64::NAN,
            DeepLayerSpec::Dense { weights, .. } => weights.clear(),
            DeepLayerSpec::BatchNorm { epsilon } => *epsilon = 0.0,
            DeepLayerSpec::Dropout { p } => *p = -1.0,
            DeepLayerSpec::CNN1D { stride, .. } => *stride = 0,
            DeepLayerSpec::CNN2D { padding_height, .. } => *padding_height = usize::MAX,
            DeepLayerSpec::Conv { hop_weights, .. } => hop_weights[0] = -1.0,
            DeepLayerSpec::Pool { pool_size, .. } => *pool_size = 0,
            DeepLayerSpec::RNN { weights_hidden, .. }
            | DeepLayerSpec::LSTM { weights_hidden, .. } => weights_hidden.clear(),
            _ => continue,
        }
        assert!(candidate.validate().is_err());
        invalid.push(candidate);
    }
    invalid.push(model(Vec::new()));
    invalid.push(model(vec![DeepLayerSpec::Flatten]));
    invalid.push(model(vec![
        DeepLayerSpec::Input { dimensions: 1 },
        DeepLayerSpec::Input { dimensions: 1 },
    ]));
    let mut alpha = small();
    alpha.alpha = f64::INFINITY;
    invalid.push(alpha);
    candidates.extend(invalid);
    for candidate in candidates {
        let expected = crate::DeepFusionOperator::new(
            candidate.to_layers().unwrap(),
            candidate.alpha,
            candidate.gating_runtime(),
        )
        .map(|_| ())
        .map_err(|error| error.to_string());
        assert_eq!(
            candidate.validate().map_err(|error| error.to_string()),
            expected
        );
    }
}
