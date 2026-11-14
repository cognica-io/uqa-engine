//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Cursor, Read};
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

struct Reader<'a> {
    source: Cursor<&'a [u8]>,
    ended: bool,
}
impl Read for Reader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        assert!(
            !self.ended,
            "evaluated-index conversion reread completed input"
        );
        let count = self.source.read(output)?;
        self.ended = count == 0;
        Ok(count)
    }
}

fn native_metadata(carried: u8) -> Vec<u8> {
    let field = "nλ".repeat(4000);
    let mut bytes = b"UQAEVI01".to_vec();
    bytes.extend_from_slice(&[carried, 1, 1, 2]);
    bytes.extend_from_slice(&2_u64.to_le_bytes());
    bytes.extend_from_slice(&(field.len() as u64).to_le_bytes());
    bytes.extend_from_slice(field.as_bytes());
    bytes
}

fn advance(
    mut input: ImageDecoder<ValueIndexImageBuilder, SQLError>,
    bytes: &[u8],
) -> (ValueIndexImageBuilder, usize) {
    let mut source = Reader {
        source: Cursor::new(bytes),
        ended: false,
    };
    let mut post_input = 0;
    let mut steps = 0;
    let value = loop {
        steps += 1;
        if source.ended {
            post_input += 1;
        }
        let maximum = if steps % 2 == 0 { 1 } else { 17 };
        let before = source.source.position();
        let next = input
            .advance(
                &mut source,
                ImageDecodeWindow {
                    bytes: maximum,
                    steps: 1,
                },
            )
            .unwrap();
        assert!(source.source.position() - before <= maximum as u64);
        if let Some(next) = next {
            break next;
        }
    };
    assert_eq!(source.source.position(), bytes.len() as u64);
    (value, post_input)
}

#[test]
fn evaluated_input_resumes_metadata_and_native_key_conversion_without_replaying_values() {
    let values = [
        (0, Value::List(vec![Value::Str("한🙂".repeat(4000)); 4])),
        (u64::MAX, Value::Int(7)),
    ];
    for carried in [0, 1] {
        let control = StorageReadControl::with_limit(16 << 20);
        let metadata = native_metadata(carried);
        let (mut builder, post) = advance(
            ValueIndexImageBuilder::image_input(16 << 20, &control).unwrap(),
            &metadata,
        );
        assert!(post >= if carried == 0 { 2 } else { 1 });
        for (id, value) in &values {
            let bytes = encode(value);
            let (next, post) = advance(builder.value_input(*id).unwrap(), &bytes);
            if *id == 0 {
                // Carried values retain payload accounting but have no physical-key copy.
                assert!(post > if carried == 0 { 30 } else { 4 });
            }
            builder = next;
        }
        let mut index = builder.finish().unwrap();
        let output = StorageReadControl::with_limit(16 << 20);
        let captured = index.capture_values(&output).unwrap();
        control.cancellation().cancel();
        assert_eq!(
            bytes(captured.metadata_image_reader(&output).unwrap()),
            metadata
        );
        for (id, value) in &values {
            assert_eq!(
                bytes(captured.value_image_reader(*id, &output).unwrap().unwrap()),
                encode(value)
            );
        }
        index.replace(7, Some(&Value::Int(3)), &output).unwrap();
        drop(index);
        assert!(control.memory().used() > 0);
        drop(captured);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(output.memory().used(), 0);
    }
}

#[test]
fn evaluated_input_retires_original_metadata_and_values_on_post_eof_failures() {
    for metadata_phase in [true, false] {
        for failure in ["cancel", "memory", "window", "drop"] {
            let control = StorageReadControl::with_limit(8 << 20);
            let (mut input, bytes) = if metadata_phase {
                (
                    ValueIndexImageBuilder::image_input(8 << 20, &control).unwrap(),
                    native_metadata(0),
                )
            } else {
                let (builder, _) = advance(
                    ValueIndexImageBuilder::image_input(8 << 20, &control).unwrap(),
                    &native_metadata(0),
                );
                (
                    builder.value_input(0).unwrap(),
                    encode(&Value::List(vec![Value::Str("한글".repeat(4000)); 8])),
                )
            };
            let mut source = Reader {
                source: Cursor::new(&bytes),
                ended: false,
            };
            while !source.ended {
                assert!(input
                    .advance(
                        &mut source,
                        ImageDecodeWindow {
                            bytes: 4096,
                            steps: 1
                        }
                    )
                    .unwrap()
                    .is_none());
            }
            let held = (failure == "memory").then(|| {
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap()
            });
            if failure == "cancel" {
                control.cancellation().cancel();
            }
            if failure != "drop" {
                loop {
                    match input.advance(
                        &mut source,
                        ImageDecodeWindow {
                            bytes: 1,
                            steps: usize::from(failure != "window"),
                        },
                    ) {
                        Ok(None) => (),
                        Ok(Some(_)) => {
                            panic!("failed evaluated input published: {metadata_phase} {failure}")
                        }
                        Err(_) => break,
                    }
                }
            }
            drop((input, held));
            assert_eq!(control.memory().used(), 0, "{metadata_phase} {failure}");
        }
    }
}

#[test]
fn borrowed_evaluated_input_preserves_original_and_invoking_cancellation_at_every_boundary() {
    for metadata_phase in [true, false] {
        let mut completed = false;
        for boundary in 0..512 {
            for cancel_original in [false, true] {
                let original = StorageReadControl::with_limit(4 << 20);
                let invoking = StorageReadControl::with_limit(4 << 20);
                let mut metadata = Some(
                    ValueIndexImageMetadata::read_image(
                        &mut native_metadata(0).as_slice(),
                        &original,
                    )
                    .unwrap(),
                );
                let builder = if metadata_phase {
                    None
                } else {
                    Some(
                        ValueIndexImageBuilder::new(metadata.take().unwrap(), 4 << 20, &original)
                            .unwrap()
                            .add_value(1, decode(&Value::Int(1), &original))
                            .unwrap(),
                    )
                };
                let value = (!metadata_phase)
                    .then(|| decode(&Value::List(vec![Value::Int(7); 8]), &original));
                let owner = original.clone();
                let mut input = ImageDecoder::<_, SQLError>::new(
                    invoking.memory(),
                    invoking.cancellation(),
                    |mut reader| async move {
                        let mut input = reader.input();
                        if let Some(metadata) = metadata {
                            ValueIndexImageBuilder::new_from(metadata, 4 << 20, &owner, &mut input)
                                .await
                        } else {
                            builder
                                .unwrap()
                                .add_value_from(9, value.unwrap(), &mut input)
                                .await
                        }
                    },
                )
                .unwrap();
                let window = ImageDecodeWindow { bytes: 1, steps: 1 };
                for _ in 0..boundary {
                    if let Some(builder) = input.advance(&mut std::io::empty(), window).unwrap() {
                        drop(builder);
                        completed = true;
                        break;
                    }
                }
                if !completed {
                    if cancel_original {
                        original.cancellation().cancel();
                    } else {
                        invoking.cancellation().cancel();
                    }
                    match input.advance(&mut std::io::empty(), window) {
                        Err(error) => assert!(error.to_string().contains("cancel"), "{error}"),
                        _ => panic!("cancellation was delayed at metadata={metadata_phase}, boundary={boundary}, original={cancel_original}"),
                    }
                    assert_eq!(original.memory().used(), 0);
                    assert_eq!(invoking.memory().used(), 0);
                    assert!(input.advance(&mut std::io::empty(), window).is_err());
                }
                drop(input);
                assert_eq!(original.memory().used(), 0);
                assert_eq!(invoking.memory().used(), 0);
            }
            if completed {
                assert!(boundary > 2);
                break;
            }
        }
        assert!(completed);
    }
}
