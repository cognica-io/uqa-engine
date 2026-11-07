//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::index::value::ValueIndexBuilder;
use uqa_core::Value;

fn bytes(mut reader: ImageReader<'_>) -> Vec<u8> {
    while !reader.prepare(1).unwrap().ready {}
    let mut output = Vec::new();
    let mut window = [0; 37];
    loop {
        let count = reader.read(&mut window).unwrap();
        if count == 0 {
            return output;
        }
        output.extend_from_slice(&window[..count]);
    }
}

#[test]
fn evaluated_index_metadata_preserves_independent_wire_and_historical_policy() {
    let control = StorageReadControl::with_limit(1 << 20);
    let source = ValueIndexBuilder::from_values(
        "nλ",
        true,
        1 << 20,
        [(0, Value::Null), (u64::MAX, Value::Float(-0.0))],
        &control,
    )
    .unwrap();
    let retained = StorageReadControl::with_limit(1 << 20);
    let (mut captured, memory) = source.capture_values(&retained).unwrap().into_parts();
    captured.policy.has_temporal = true;
    captured.policy.has_row_values = true;
    captured.policy.comparison = ValueIndexComparison::Fallible;
    drop(source);
    let output = bytes(captured.metadata_image_reader(&control).unwrap());
    let mut expected =
        b"UQAEVI01\x01\x01\x01\x02\x02\x00\x00\x00\x00\x00\x00\x00\x03\x00\x00\x00\x00\x00\x00\x00"
            .to_vec();
    expected.extend_from_slice("nλ".as_bytes());
    assert_eq!(output, expected);
    let metadata = ValueIndexImageMetadata::read_image(&mut output.as_slice(), &control).unwrap();
    assert_eq!(&*metadata.field, "nλ");
    assert_eq!(metadata.len, 2);
    assert_eq!(metadata.policy, captured.policy().unwrap());
    for prefix in 0..output.len() {
        let input = StorageReadControl::with_limit(1 << 20);
        assert!(ValueIndexImageMetadata::read_image(&mut &output[..prefix], &input).is_err());
        assert_eq!(input.memory().used(), 0);
    }
    for position in [0, 8, 9, 10, 11] {
        let mut invalid = output.clone();
        invalid[position] = 0xff;
        assert!(ValueIndexImageMetadata::read_image(&mut invalid.as_slice(), &control).is_err());
    }
    let mut trailing = output.clone();
    trailing.push(0);
    assert!(ValueIndexImageMetadata::read_image(&mut trailing.as_slice(), &control).is_err());
    let small = StorageReadControl::with_limit(2);
    assert!(matches!(
        ValueIndexImageMetadata::read_image(&mut output.as_slice(), &small),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(small.memory().used(), 0);
    drop((metadata, captured, memory));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(retained.memory().used(), 0);
}

#[test]
fn evaluated_index_value_images_preserve_exact_carriers_and_deleted_positions() {
    let control = StorageReadControl::with_limit(8 << 20);
    let mut images = Vec::new();
    for allowance in [0, 1 << 20] {
        let mut source = ValueIndexBuilder::from_values(
            "stored",
            false,
            allowance,
            [
                (0, Value::Null),
                (7, Value::Int(9)),
                (u64::MAX, Value::Float(-0.0)),
            ],
            &control,
        )
        .unwrap();
        source.replace(7, None, &control).unwrap();
        let retained = StorageReadControl::with_limit(1 << 20);
        let captured = source.capture_values(&retained).unwrap();
        source.replace(0, Some(&Value::Int(5)), &control).unwrap();
        source.clear();
        drop(source);
        let mut after = None;
        let mut positions = Vec::new();
        let mut selected = Vec::new();
        while let Some((id, live)) = captured.next_image_position(after, &control).unwrap() {
            assert!(after.is_none_or(|previous| id > previous));
            after = Some(id);
            positions.push((id, live));
            let reader = captured.value_image_reader(id, &control).unwrap();
            assert_eq!(reader.is_some(), live);
            if let Some(reader) = reader {
                selected.push((id, bytes(reader)));
            }
        }
        assert_eq!(captured.len().unwrap(), 2);
        if allowance == 0 {
            assert_eq!(positions, [(0, true), (7, false), (u64::MAX, true)]);
        } else {
            assert_eq!(positions, [(0, true), (u64::MAX, true)]);
        }
        assert!(captured.value_image_reader(8, &control).unwrap().is_none());
        images.push(selected);
        drop(captured);
        assert_eq!(retained.memory().used(), 0);
    }
    assert_eq!(images[0], images[1]);
    for (id, image) in &images[0] {
        let restored = ImageInput {
            reader: &mut image.as_slice(),
            control: &control,
        }
        .value()
        .unwrap();
        if *id == 0 {
            assert!(matches!(&*restored, Value::Null));
        } else {
            let Value::Float(value) = &*restored else {
                panic!("original float")
            };
            assert_eq!(value.to_bits(), (-0.0f64).to_bits());
        }
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn evaluated_index_images_stream_wide_values_with_independent_bounded_workspace() {
    let original = Value::Bytes(vec![0x5a; 2 << 20]);
    for allowance in [0, 16 << 20] {
        let owner = StorageReadControl::with_limit(16 << 20);
        let source = ValueIndexBuilder::from_values(
            "wide",
            false,
            allowance,
            [(9, original.clone())],
            &owner,
        )
        .unwrap();
        let capture =
            StorageReadControl::with_limit(if allowance == 0 { 128 << 10 } else { 8 << 20 });
        let captured = source.capture_values(&capture).unwrap();
        drop(source);
        let output = StorageReadControl::with_limit(128 << 10);
        let mut encoded = Vec::with_capacity((2 << 20) + 64);
        let measured = allocation_counter::measure(|| {
            let mut reader = captured.value_image_reader(9, &output).unwrap().unwrap();
            loop {
                let progress = reader.prepare(1).unwrap();
                assert!(progress.positions <= 1);
                if progress.ready {
                    break;
                }
                assert_eq!(progress.positions, 1);
            }
            let mut buffer = [0; 16 << 10];
            loop {
                let count = reader.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                if allowance == 0 {
                    assert!(count <= 4096);
                }
                encoded.extend_from_slice(&buffer[..count]);
            }
        });
        assert!(measured.bytes_max < 128 << 10, "{allowance}: {measured:?}");
        assert_eq!(output.memory().used(), 0);
        let restored = ImageInput {
            reader: &mut encoded.as_slice(),
            control: &owner,
        }
        .value()
        .unwrap();
        assert_eq!(&*restored, &original);
        drop((restored, captured));
        assert_eq!(capture.memory().used(), 0);
        assert_eq!(owner.memory().used(), 0);
    }
}

#[test]
fn evaluated_index_image_readers_keep_both_controls_and_release_failed_workspace() {
    for value_image in [false, true] {
        for cancel_capture in [false, true] {
            let owner = StorageReadControl::with_limit(1 << 20);
            let source = ValueIndexBuilder::from_values(
                "stored",
                false,
                1 << 20,
                [(1, Value::Null)],
                &owner,
            )
            .unwrap();
            let capture = StorageReadControl::with_limit(1 << 20);
            let captured = source.capture_values(&capture).unwrap();
            let output = StorageReadControl::with_limit(128 << 10);
            let mut reader = if value_image {
                captured.value_image_reader(1, &output).unwrap().unwrap()
            } else {
                captured.metadata_image_reader(&output).unwrap()
            };
            assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
            if cancel_capture {
                capture.cancellation().cancel();
            } else {
                output.cancellation().cancel();
            }
            assert!(matches!(
                reader.read(&mut [0; 1]),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert_eq!(output.memory().used(), 0);
            assert!(reader.read(&mut [0; 1]).is_err());
            drop(reader);
            drop((captured, source));
            assert_eq!(capture.memory().used(), 0);
            assert_eq!(owner.memory().used(), 0);
        }
    }
}
