//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::native::{encode_row, NativeRecord, NativeRecordOwner};

fn record(binding: Option<&str>) -> NativeRecord {
    NativeRecord::encode(
        Family::TableFieldAnalyzers,
        NativeRecordOwner::Object {
            identity: [1; 16],
            generation: [2; 16],
        },
        &[
            ValueRef::Text(b"\"Schema.Table\""),
            ValueRef::Text("field\0한글".as_bytes()),
            ValueRef::Text(b"Search"),
            ValueRef::Text(b""),
            binding.map_or(ValueRef::Null, |text| ValueRef::Text(text.as_bytes())),
        ],
        &StorageReadControl::with_limit(1 << 20),
    )
    .unwrap()
}

#[test]
fn native_analyzer_input_preserves_object_owned_fields_and_absent_binding_rows() {
    for json in [None, Some(""), Some(" { \"x\": 1.00 } ")] {
        let record = record(json);
        let control = StorageReadControl::with_limit(65536);
        let restored = binding(record.key(), record.row(), &control).unwrap();
        assert_eq!(restored.as_ref().map(|row| row.binding_json.as_str()), json);
        if let Some(row) = &restored {
            assert_eq!(row.table, "\"Schema.Table\"");
            assert_eq!(row.field, "field\0한글");
        }
        let label = label(record.key(), record.row(), &control)
            .unwrap()
            .unwrap();
        assert_eq!(label.table, "\"Schema.Table\"");
        assert_eq!(label.field, "field\0한글");
        assert_eq!(label.phase, "Search");
        assert_eq!(label.analyzer_name, "");
        assert_eq!(
            control.memory().used(),
            label.reserved_bytes() + restored.as_ref().map_or(0, Budgeted::reserved_bytes)
        );
        drop((restored, label));
        assert_eq!(control.memory().used(), 0);
    }
}

fn same_record_error(key: &[u8], bytes: &[u8]) {
    let control = StorageReadControl::with_limit(65536);
    let expected = decode_record(key, bytes, &control)
        .unwrap_err()
        .into_storage_error()
        .to_string();
    assert_eq!(
        binding(key, bytes, &control).unwrap_err().to_string(),
        expected
    );
    assert_eq!(
        label(key, bytes, &control).unwrap_err().to_string(),
        expected
    );
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_analyzer_input_keeps_original_record_validation_before_field_projection() {
    let record = record(Some("{}"));
    for length in 0..record.key().len() {
        same_record_error(&record.key()[..length], record.row());
    }
    for length in 0..record.row().len() {
        same_record_error(record.key(), &record.row()[..length]);
    }
    let control = StorageReadControl::with_limit(65536);
    let mut key = record.key().to_vec();
    key.push(0);
    same_record_error(&key, record.row());
    for column in 0..5 {
        for invalid in [
            ValueRef::Null,
            ValueRef::Integer(7),
            ValueRef::Blob(b"wrong storage class"),
        ] {
            if column == 4 && invalid == ValueRef::Null {
                continue;
            }
            let (_, mut fields) = decode_record(record.key(), record.row(), &control).unwrap();
            fields[column] = invalid;
            let bytes = encode_row(&fields, &control).unwrap();
            same_record_error(record.key(), &bytes);
        }
    }
    let (_, mut fields) = decode_record(record.key(), record.row(), &control).unwrap();
    fields[1] = ValueRef::Text(b"another field");
    let bytes = encode_row(&fields, &control).unwrap();
    same_record_error(record.key(), &bytes);
    drop((fields, bytes));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_analyzer_input_admits_native_fields_and_retires_partial_conversion() {
    let json = "{ \"source\": \"한글\" } ".repeat(4096);
    let record = record(Some(&json));
    let control = StorageReadControl::with_limit(1 << 20);
    let mut restored = None;
    let measured = allocation_counter::measure(|| {
        restored = Some(
            binding(record.key(), record.row(), &control)
                .unwrap()
                .unwrap(),
        );
    });
    let restored = restored.unwrap();
    assert_eq!(
        measured.bytes_current,
        i64::try_from(control.memory().used()).unwrap()
    );
    assert_eq!(restored.binding_json, json);
    let peak = control.memory().peak();
    drop(restored);
    for limit in (0..peak).step_by(1024).chain([peak - 1]) {
        let denied = StorageReadControl::with_limit(limit);
        assert!(binding(record.key(), record.row(), &denied).is_err());
        assert_eq!(denied.memory().used(), 0);
    }
    control.cancellation().cancel();
    assert!(binding(record.key(), record.row(), &control).is_err());
    assert!(label(record.key(), record.row(), &control).is_err());
    assert_eq!(control.memory().used(), 0);
}
