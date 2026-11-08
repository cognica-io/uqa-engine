//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{value_image::write_value_image, Predicate};
use uqa_storage::image_io::{ImageInput, ImageReader};

fn metadata_bytes(flags: [u8; 4], len: u64) -> Vec<u8> {
    let mut bytes = b"UQAEVI01".to_vec();
    bytes.extend_from_slice(&flags);
    bytes.extend_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(&3_u64.to_le_bytes());
    bytes.extend_from_slice("nλ".as_bytes());
    bytes
}

fn metadata(flags: [u8; 4], len: u64, input: &StorageReadControl) -> ValueIndexImageMetadata {
    ValueIndexImageMetadata::read_image(&mut metadata_bytes(flags, len).as_slice(), input).unwrap()
}

fn encode(value: &Value) -> Vec<u8> {
    let output = StorageReadControl::with_limit(1 << 20);
    let mut bytes = vec![];
    write_value_image(value, &mut bytes, output.memory(), output.cancellation()).unwrap();
    bytes
}

fn decode(value: &Value, input: &StorageReadControl) -> Budgeted<Value> {
    ImageInput {
        reader: &mut encode(value).as_slice(),
        control: input,
    }
    .value()
    .unwrap()
}

fn bytes(mut reader: ImageReader<'_>) -> Vec<u8> {
    while !reader.prepare(1).unwrap().ready {}
    let mut bytes = vec![];
    let mut buffer = [0; 29];
    loop {
        let n = reader.read(&mut buffer).unwrap();
        if n == 0 {
            return bytes;
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
}

#[test]
fn native_evaluated_input_preserves_independent_policy_bytes_and_original_value_buffers() {
    let values = [
        (0, Value::Null),
        (7, Value::Str("original".into())),
        (19, Value::Float(-0.0)),
        (
            u64::MAX,
            Value::Float(f64::from_bits(0x7ff8_0000_0000_0042)),
        ),
    ];
    for carried in 0..2 {
        for temporal in 0..2 {
            for row in 0..2 {
                for comparison in 0..3 {
                    let flags = [carried, temporal, row, comparison];
                    let input = StorageReadControl::with_limit(1 << 20);
                    let output = StorageReadControl::with_limit(1 << 20);
                    let source = metadata(flags, 4, &input);
                    let field_pointer = source.field.as_ptr();
                    let mut builder = ValueIndexImageBuilder::new(source, 1 << 20, &input).unwrap();
                    let mut pointer = None;
                    for (id, value) in &values {
                        let decoded = decode(value, &input);
                        if let Value::Str(text) = &*decoded {
                            pointer = Some(text.as_ptr());
                        }
                        builder = builder.add_value(*id, decoded).unwrap();
                    }
                    let index = builder.finish().unwrap();
                    assert_eq!(index.field.as_ptr(), field_pointer);
                    let original = index.stored_value(7, &output).unwrap().unwrap();
                    let Value::Str(text) = &*original else {
                        panic!("stored string")
                    };
                    assert_eq!(Some(text.as_ptr()), pointer);
                    drop(original);
                    input.cancellation().cancel();
                    let captured = index.capture_values(&output).unwrap();
                    drop(index);
                    assert!(input.memory().used() > 0);
                    assert_eq!(
                        bytes(captured.metadata_image_reader(&output).unwrap()),
                        metadata_bytes(flags, 4)
                    );
                    for (id, value) in &values {
                        assert_eq!(
                            bytes(captured.value_image_reader(*id, &output).unwrap().unwrap()),
                            encode(value)
                        );
                    }
                    drop(captured);
                    assert_eq!(input.memory().used(), 0);
                    assert_eq!(output.memory().used(), 0);
                }
            }
        }
    }
}

#[test]
fn native_evaluated_input_preserves_probes_and_historical_policy_through_mutation_and_spill() {
    let input = StorageReadControl::with_limit(1 << 20);
    let output = StorageReadControl::with_limit(1 << 20);
    let values = [
        (0, Value::Null),
        (7, Value::Int(1)),
        (19, Value::Int(1)),
        (u64::MAX, Value::Int(9)),
    ];
    let mut builder =
        ValueIndexImageBuilder::new(metadata([0; 4], 4, &input), 1 << 20, &input).unwrap();
    for (id, value) in &values {
        builder = builder.add_value(*id, decode(value, &input)).unwrap();
    }
    let mut index = builder.finish().unwrap();
    let reference = ColumnValueIndex::build("nλ", values.into_iter());
    for predicate in [
        Predicate::Equals(Value::Int(1)),
        Predicate::GreaterThan(Value::Int(1)),
        Predicate::LessThanOrEqual(Value::Int(9)),
        Predicate::Between {
            low: Value::Int(1),
            high: Value::Int(9),
        },
        Predicate::IsNull,
        Predicate::IsNotNull,
    ] {
        assert_eq!(
            index
                .scan_observing_with_catalog(&predicate, None, &output, || Ok(()))
                .unwrap(),
            reference.scan(&predicate)
        );
        assert_eq!(
            index.estimate_cardinality(&predicate, &output).unwrap(),
            reference.estimate_cardinality(&predicate)
        );
    }
    let before = index.capture_values(&output).unwrap();
    input.cancellation().cancel();
    index.replace(7, Some(&Value::Int(5)), &output).unwrap();
    assert_eq!(
        &**before.value(7).unwrap().as_ref().unwrap(),
        &Value::Int(1)
    );
    assert_eq!(
        &*index.stored_value(7, &output).unwrap().unwrap(),
        &Value::Int(5)
    );
    drop((index, before));
    assert_eq!(input.memory().used(), 0);
    for carried in [0, 1] {
        let input = StorageReadControl::with_limit(1 << 20);
        let mut index =
            ValueIndexImageBuilder::new(metadata([carried, 1, 1, 2], 1, &input), 0, &input)
                .unwrap()
                .add_value(7, decode(&Value::Int(3), &input))
                .unwrap()
                .finish()
                .unwrap();
        let expected = index.capture_values(&output).unwrap().policy().unwrap();
        input.cancellation().cancel();
        index.replace(9, Some(&Value::Int(5)), &output).unwrap();
        assert!(index.is_spilled());
        assert_eq!(
            index.capture_values(&output).unwrap().policy().unwrap(),
            expected
        );
        assert_eq!(input.memory().used(), 0);
        drop(index);
    }
    assert_eq!(output.memory().used(), 0);
}

#[test]
fn native_evaluated_input_rejects_partial_foreign_unordered_unfunded_and_cancelled_owners() {
    for failure in 0..6 {
        let input = StorageReadControl::with_limit(1 << 20);
        let foreign = StorageReadControl::with_limit(1 << 20);
        let builder = ValueIndexImageBuilder::new(metadata([0; 4], 2, &input), 1024, &input)
            .unwrap()
            .add_value(7, decode(&Value::Int(1), &input))
            .unwrap();
        if failure == 0 {
            assert!(builder.finish().is_err());
            assert_eq!(input.memory().used(), 0);
            continue;
        }
        let value = match failure {
            1 => decode(&Value::Int(2), &foreign),
            3 => Budgeted::new(
                Value::Str("unfunded".into()),
                input.memory().empty_reservation(),
            ),
            _ => decode(&Value::Int(2), &input),
        };
        let occupied =
            (failure == 4).then(|| input.memory().reserve(input.memory().available()).unwrap());
        if failure == 5 {
            input.cancellation().cancel();
        }
        assert!(builder
            .add_value(if failure == 2 { 7 } else { 9 }, value)
            .is_err());
        assert_eq!(
            input.memory().used(),
            occupied
                .as_ref()
                .map_or(0, uqa_core::memory::MemoryReservation::bytes)
        );
        assert_eq!(foreign.memory().used(), 0);
        drop(occupied);
        assert_eq!(input.memory().used(), 0);
    }
    let input = StorageReadControl::with_limit(1 << 20);
    let builder = ValueIndexImageBuilder::new(metadata([0; 4], 1, &input), 1024, &input).unwrap();
    assert!(builder
        .add_value(1, decode(&Value::Row(vec![Value::Null].into()), &input))
        .is_err());
    assert_eq!(input.memory().used(), 0);
}

#[test]
fn native_evaluated_mutations_keep_original_owners_under_exhausted_input_admission() {
    let input = StorageReadControl::with_limit(1 << 20);
    let output = StorageReadControl::with_limit(1 << 20);
    let builder = ValueIndexImageBuilder::new(metadata([0; 4], 1, &input), 1 << 20, &input)
        .unwrap()
        .add_value(7, decode(&Value::Str("original".into()), &input))
        .unwrap();
    let mut index = builder.finish().unwrap();
    let occupied = input.memory().reserve(input.memory().available()).unwrap();
    let retained = index.capture_values(&output).unwrap();
    input.cancellation().cancel();
    index
        .replace(9, Some(&Value::Str("new".into())), &output)
        .unwrap();
    index.replace(7, None, &output).unwrap();
    assert!(index.stored_value(7, &output).unwrap().is_none());
    assert_eq!(
        &**retained.value(7).unwrap().as_ref().unwrap(),
        &Value::Str("original".into())
    );
    drop((index, retained));
    assert_eq!(input.memory().used(), occupied.bytes());
    assert_eq!(output.memory().used(), 0);
    drop(occupied);
    assert_eq!(input.memory().used(), 0);
}

#[test]
fn native_carried_input_preserves_unobserved_policy_and_later_search_key_conversion() {
    use crate::catalog::index::value::ValueIndexBuilder;
    let input = StorageReadControl::with_limit(1 << 20);
    let output = StorageReadControl::with_limit(1 << 20);
    let values = [
        (1, Value::Row(vec![Value::Int(3), Value::Null].into())),
        (
            9,
            Value::Temporal(uqa_core::TemporalValue::parse_date("2026-01-02").unwrap()),
        ),
    ];
    let mut native =
        ValueIndexBuilder::from_values("nλ", false, 1 << 20, values.clone(), &output).unwrap();
    native.set_use("nλ", true);
    let source = native.capture_values(&output).unwrap();
    assert_eq!(
        bytes(source.metadata_image_reader(&output).unwrap()),
        metadata_bytes([1, 0, 0, 0], 2)
    );
    let mut builder =
        ValueIndexImageBuilder::new(metadata([1, 0, 0, 0], 2, &input), 1 << 20, &input).unwrap();
    for (id, value) in &values {
        builder = builder.add_value(*id, decode(value, &input)).unwrap();
    }
    let mut restored = builder.finish().unwrap();
    input.cancellation().cancel();
    let captured = restored.capture_values(&output).unwrap();
    assert_eq!(
        bytes(captured.metadata_image_reader(&output).unwrap()),
        metadata_bytes([1, 0, 0, 0], 2)
    );
    for (id, value) in &values {
        assert_eq!(
            bytes(captured.value_image_reader(*id, &output).unwrap().unwrap()),
            encode(value)
        );
    }
    for predicate in [Predicate::IsNull, Predicate::Equals(Value::Int(3))] {
        assert!(!restored.supports(&predicate));
    }
    native.set_use("nλ", false);
    restored.set_use("nλ", false);
    assert_eq!(
        restored.capture_values(&output).unwrap().policy().unwrap(),
        native.capture_values(&output).unwrap().policy().unwrap()
    );
    for predicate in [Predicate::IsNull, Predicate::Equals(Value::Int(3))] {
        assert_eq!(restored.supports(&predicate), native.supports(&predicate));
    }
    drop((source, captured, native, restored));
    assert_eq!(input.memory().used(), 0);
    assert_eq!(output.memory().used(), 0);
}
