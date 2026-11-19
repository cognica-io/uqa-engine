//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

struct Reader<'a> {
    source: &'a [u8],
    bytes: usize,
    calls: usize,
    eof: bool,
}

impl<'a> Reader<'a> {
    fn new(source: &'a [u8]) -> Self {
        Self {
            source,
            bytes: 0,
            calls: 0,
            eof: false,
        }
    }
}

impl Read for Reader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        assert!(!self.eof, "completed model input was replayed");
        assert!(!bytes.is_empty());
        self.calls += 1;
        let count = self.source.len().min(bytes.len());
        bytes[..count].copy_from_slice(&self.source[..count]);
        self.source = &self.source[count..];
        self.bytes += count;
        self.eof = count == 0;
        Ok(count)
    }
}

fn decoder(control: &StorageReadControl) -> ImageDecoder<Budgeted<DeepModel>, StorageBackendError> {
    ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut channel| async move { read_model_image_from(&mut channel.input()).await },
    )
    .unwrap()
}

fn literal() -> Vec<u8> {
    let mut bytes = b"UQAMOD01".to_vec();
    bytes.extend(0x8000_0000_0000_0000_u64.to_le_bytes());
    bytes.push(2);
    bytes.extend(1_u64.to_le_bytes());
    bytes.push(1);
    bytes.extend(8193_u64.to_le_bytes());
    for index in 0..8193 {
        let bits = if index % 2 == 0 {
            0x8000_0000_0000_0000_u64
        } else {
            1
        };
        bytes.extend(bits.to_le_bytes());
    }
    bytes
}

fn named_layers() -> Vec<u8> {
    let mut layers = vec![DeepLayerSpec::Input { dimensions: 1 }];
    for _ in 0..65 {
        layers.push(DeepLayerSpec::Propagate {
            edge_label: "한글\0α".repeat(1025),
            aggregation: crate::AggregationSpec::Mean,
            direction: crate::DirectionSpec::Both,
        });
    }
    encode(&model(layers))
}

#[test]
fn model_input_retains_current_windows_exact_bits_and_post_eof_validation() {
    for bytes in [literal(), named_layers()] {
        let control = StorageReadControl::with_limit(8 << 20);
        let mut input = decoder(&control);
        let mut reader = Reader::new(&bytes);
        let mut calls = 0;
        let mut post_eof = 0;
        let restored = loop {
            let window = ImageDecodeWindow {
                bytes: [1, 7, 4096][calls % 3],
                steps: 1,
            };
            let before = (reader.bytes, reader.calls);
            post_eof += usize::from(reader.eof);
            let result = input.advance(&mut reader, window).unwrap();
            assert!(reader.bytes - before.0 <= window.bytes);
            assert!(reader.calls - before.1 <= window.steps);
            calls += 1;
            assert!(calls < bytes.len() * 8);
            if let Some(model) = result {
                break model;
            }
        };
        assert!(reader.eof && post_eof > 16);
        assert_eq!(reader.bytes, bytes.len());
        assert_eq!(encode(&restored), bytes);
        let ordinary = StorageReadControl::with_limit(8 << 20);
        let synchronous = read_model_image(&mut bytes.as_slice(), &ordinary).unwrap();
        assert_eq!(control.memory().used(), ordinary.memory().used());
        drop(synchronous);
        assert_eq!(ordinary.memory().used(), 0);
        assert!(input
            .advance(&mut reader, ImageDecodeWindow { bytes: 1, steps: 1 })
            .is_err());
        control.cancellation().cancel();
        assert_eq!(encode(&restored), bytes);
        drop(restored);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn model_input_retires_partial_fields_and_post_eof_candidates() {
    let bytes = literal();
    for post_eof in [false, true] {
        for mode in 0..3 {
            let control = StorageReadControl::with_limit(1 << 20);
            let mut input = decoder(&control);
            let mut reader = Reader::new(&bytes);
            loop {
                assert!(input
                    .advance(
                        &mut reader,
                        ImageDecodeWindow {
                            bytes: 127,
                            steps: 1
                        }
                    )
                    .unwrap()
                    .is_none());
                if if post_eof {
                    reader.eof
                } else {
                    reader.bytes >= 40
                } {
                    break;
                }
            }
            assert!(control.memory().used() > 0);
            if mode == 0 {
                control.cancellation().cancel();
            }
            if mode != 1 {
                assert!(input
                    .advance(
                        &mut reader,
                        ImageDecodeWindow {
                            bytes: usize::from(mode != 2),
                            steps: 1
                        }
                    )
                    .is_err());
                assert_eq!(control.memory().used(), 0);
                assert!(input
                    .advance(&mut reader, ImageDecodeWindow { bytes: 1, steps: 1 })
                    .is_err());
            }
            drop(input);
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn model_input_rejects_malformed_truncated_and_unfunded_images_without_publication() {
    let bytes = literal();
    let mut invalid = Vec::new();
    for end in [0, 7, 8, 16, 17, 24, 25, 26, 33, 34, bytes.len() - 1] {
        invalid.push(bytes[..end].to_vec());
    }
    for offset in [0, 16, 25] {
        let mut image = bytes.clone();
        image[offset] = 255;
        invalid.push(image);
    }
    for offset in [17, 26] {
        let mut image = bytes.clone();
        image[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        invalid.push(image);
    }
    for offset in [8, 34] {
        let mut image = bytes.clone();
        image[offset..offset + 8].copy_from_slice(&f64::NAN.to_bits().to_le_bytes());
        invalid.push(image);
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    invalid.push(trailing);
    for image in invalid {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = decoder(&control);
        let mut reader = Reader::new(&image);
        let error = loop {
            match input.advance(
                &mut reader,
                ImageDecodeWindow {
                    bytes: 4096,
                    steps: 1,
                },
            ) {
                Ok(None) => (),
                Ok(Some(_)) => panic!("invalid model was published"),
                Err(error) => break error,
            }
        };
        let expected = read_model_image(&mut image.as_slice(), &control).unwrap_err();
        assert_eq!(error.to_string(), expected.to_string());
        assert_eq!(control.memory().used(), 0);
        assert!(input
            .advance(&mut reader, ImageDecodeWindow { bytes: 1, steps: 1 })
            .is_err());
    }
    let denied = StorageReadControl::with_limit(32768);
    let input = decoder(&denied);
    assert!(matches!(
        input.read(&mut bytes.as_slice()),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(denied.memory().used(), 0);
}
