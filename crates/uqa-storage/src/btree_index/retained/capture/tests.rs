//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::records::{chunk_key, CHUNK};
use super::*;
use crate::mvcc::RecordWrite;

fn encode(value: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    let control = StorageReadControl::with_limit(64 << 10);
    uqa_core::value_image::write_value_image(
        value,
        &mut bytes,
        control.memory(),
        control.cancellation(),
    )
    .unwrap();
    bytes
}

#[test]
fn native_value_capture_streams_wide_values_with_bounded_memory_after_source_loss() {
    let value = Value::Bytes(vec![0x5a; 2 << 20]);
    let mut expected = b"UQAVAL01".to_vec();
    // Core's bytes carrier is tag 7 followed by its little-endian byte length.
    expected.push(7);
    expected.extend_from_slice(&(2_u64 << 20).to_le_bytes());
    expected.extend_from_slice(&vec![0x5a; 2 << 20]);
    let write = StorageReadControl::with_limit(4 << 20);
    let mut source = RetainedValueIndex::new(0);
    source.replace(7, Some(&value), &write).unwrap();
    let capture = StorageReadControl::with_limit(128 << 10);
    let mut saved = None;
    let allocated = allocation_counter::measure(|| saved = Some(source.capture(&capture).unwrap()));
    assert!(allocated.bytes_total < 2048, "{allocated:?}");
    assert!(capture.memory().used() < 128 << 10);
    let saved = saved.unwrap();
    source.replace(7, Some(&Value::Null), &write).unwrap();
    source.clear();
    drop(source);
    assert_eq!(saved.len().unwrap(), 1);
    let small = StorageReadControl::with_limit(64 << 10);
    assert!(matches!(
        saved.get(7, &small),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(small.memory().used(), 0);
    let output = StorageReadControl::with_limit(64 << 10);
    let mut reader = saved.value_image_reader(7, &output).unwrap().unwrap();
    let mut positions = 0;
    loop {
        let progress = reader.prepare(3).unwrap();
        assert!(progress.positions <= 3);
        positions += progress.positions;
        if progress.ready {
            break;
        }
        assert!(progress.positions > 0);
    }
    assert_eq!(positions, expected.len().div_ceil(CHUNK) + 1);
    assert_eq!(reader.prepare(1).unwrap().positions, 0);
    let mut actual = Vec::new();
    let mut bytes = [0; 257];
    loop {
        let count = reader.read(&mut bytes).unwrap();
        if count == 0 {
            break;
        }
        actual.extend_from_slice(&bytes[..count]);
    }
    assert_eq!(actual, expected);
    assert_eq!(output.memory().used(), 0);
    drop(reader);
    drop(saved);
    assert_eq!(capture.memory().used(), 0);
}

#[test]
fn native_value_capture_preserves_exact_representations_and_owned_decoding() {
    let values = [
        Value::Null,
        Value::Float(-0.0),
        Value::Float(f64::from_bits(0xfff8_0000_0000_0001)),
        Value::Map(
            [
                ("$uqa_type".into(), Value::Str("bytes".into())),
                ("hex".into(), Value::Str("00ff".into())),
            ]
            .into(),
        ),
        Value::Record(
            vec![
                ("same".into(), Value::Int(1)),
                ("same".into(), Value::Int(2)),
            ]
            .into(),
        ),
    ];
    for allowance in [0, 1 << 20] {
        let write = StorageReadControl::with_limit(1 << 20);
        let mut source = RetainedValueIndex::new(allowance);
        for (id, value) in values.iter().enumerate() {
            source.replace(id as u64, Some(value), &write).unwrap();
        }
        let capture = StorageReadControl::with_limit(1 << 20);
        let saved = source.capture(&capture).unwrap();
        let charged = capture.memory().used();
        let exact = StorageReadControl::with_limit(charged);
        drop(source.capture(&exact).unwrap());
        assert_eq!(exact.memory().used(), 0);
        let denied = StorageReadControl::with_limit(charged - 1);
        assert!(matches!(
            source.capture(&denied),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(denied.memory().used(), 0);
        drop(source);
        let read = StorageReadControl::with_limit(1 << 20);
        assert!(saved.get(99, &read).unwrap().is_none());
        let mut last = None;
        for (id, expected) in values.iter().enumerate() {
            let position = saved.next_position(last, &read).unwrap().unwrap();
            assert_eq!(position, (id as u64, true));
            last = Some(position.0);
            let actual = saved.get(id as u64, &read).unwrap().unwrap();
            assert!(actual.has_same_representation(expected));
            assert_eq!(encode(&actual), encode(expected));
        }
        assert!(saved.next_position(last, &read).unwrap().is_none());
        let value = saved.get(3, &read).unwrap().unwrap();
        drop(saved);
        assert_eq!(capture.memory().used(), 0);
        assert!(read.memory().used() > 0);
        assert!(value.has_same_representation(&values[3]));
        drop(value);
        assert_eq!(read.memory().used(), 0);
    }
}

#[test]
fn native_value_image_preparation_counts_deleted_chunks_and_node_positions() {
    let write = StorageReadControl::with_limit(1 << 20);
    let mut source = RetainedValueIndex::new(0);
    let value = Value::Bytes(vec![7; 128 << 10]);
    let chunks = encode(&value).len().div_ceil(CHUNK);
    source.replace(4, Some(&value), &write).unwrap();
    source.replace(4, Some(&Value::Null), &write).unwrap();
    source.replace(0, Some(&Value::Int(1)), &write).unwrap();
    source.replace(0, None, &write).unwrap();
    let capture = StorageReadControl::with_limit(1 << 20);
    let saved = source.capture(&capture).unwrap();
    let output = StorageReadControl::with_limit(64 << 10);
    assert_eq!(
        saved.next_position(None, &output).unwrap(),
        Some((0, false))
    );
    assert_eq!(
        saved.next_position(Some(0), &output).unwrap(),
        Some((4, true))
    );
    let mut reader = saved.value_image_reader(4, &output).unwrap().unwrap();
    for _ in 0..chunks {
        let step = reader.prepare(1).unwrap();
        assert_eq!(step.positions, 1);
        assert!(!step.ready);
    }
    assert!(reader.prepare(1).unwrap().ready);
    let mut bytes = [0; 32];
    let count = reader.read(&mut bytes).unwrap();
    assert_eq!(&bytes[..count], b"UQAVAL01\0");
    assert_eq!(reader.read(&mut bytes).unwrap(), 0);
    assert_eq!(output.memory().used(), 0);
}

#[test]
fn native_value_images_reject_chunk_corruption_before_output() {
    for corrupt in [None, Some(&[][..]), Some(&[0; 2][..])] {
        let write = StorageReadControl::with_limit(1 << 20);
        let mut source = RetainedValueIndex::new(0);
        source.replace(2, Some(&Value::Null), &write).unwrap();
        source
            .records
            .apply(
                &[RecordWrite {
                    key: &chunk_key(2, 0),
                    expected: None,
                    value: corrupt,
                }],
                &write,
            )
            .unwrap();
        let capture = StorageReadControl::with_limit(1 << 20);
        let saved = source.capture(&capture).unwrap();
        let output = StorageReadControl::with_limit(64 << 10);
        let mut reader = saved.value_image_reader(2, &output).unwrap().unwrap();
        assert!(reader.prepare(8).is_err());
        assert_eq!(output.memory().used(), 0);
        assert!(reader.read(&mut [0; 16]).is_err());
        assert!(saved.get(2, &output).is_err());
        assert_eq!(output.memory().used(), 0);
    }
}

#[test]
fn native_value_images_preserve_original_and_invoking_cancellation() {
    for original in [false, true] {
        let write = StorageReadControl::with_limit(1 << 20);
        let mut source = RetainedValueIndex::new(0);
        source
            .replace(0, Some(&Value::Bytes(vec![0; 8 << 10])), &write)
            .unwrap();
        let capture = StorageReadControl::with_limit(1 << 20);
        let saved = source.capture(&capture).unwrap();
        let output = StorageReadControl::with_limit(64 << 10);
        let mut reader = saved.value_image_reader(0, &output).unwrap().unwrap();
        while !reader.prepare(1).unwrap().ready {}
        assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
        if original {
            capture.cancellation().cancel();
        } else {
            output.cancellation().cancel();
        }
        assert!(matches!(
            reader.read(&mut [0; 1]),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(output.memory().used(), 0);
        assert!(reader.prepare(1).is_err());
        drop(reader);
        assert!(source.get(0, &write).unwrap().is_some());
        drop(saved);
        assert_eq!(capture.memory().used(), 0);
    }
}
