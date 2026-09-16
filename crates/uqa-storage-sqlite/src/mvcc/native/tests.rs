//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeSet;

use rusqlite::types::ValueRef;
use uqa_storage::mvcc::{DatabaseId, VersionError};
use uqa_storage::read_control::StorageReadControl;

use super::*;
use crate::{Catalog, ManagedConnection};

mod accelerators;
mod generations;
mod graph_lookup;
mod materialization;
mod migration;
mod occurrence_guards;
mod persistence;

fn owner() -> NativeRecordOwner {
    NativeRecordOwner::Object {
        identity: [3; 16],
        generation: [7; 16],
    }
}

#[test]
fn native_layout_inventory_covers_every_current_catalog_table_and_primary_key() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .with(|connection| {
            let mut statement = connection.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT GLOB 'sqlite_*' ORDER BY name")?;
            let actual = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<BTreeSet<_>, _>>()?;
            let expected: BTreeSet<_> = NativeRecordFamily::all()
                .filter(|family| !matches!(family, NativeRecordFamily::TableOwners | NativeRecordFamily::GraphLookups | NativeRecordFamily::OccurrenceSkips | NativeRecordFamily::OccurrenceBlockMax | NativeRecordFamily::OccurrenceGuards))
                .map(|family| family.layout().table.to_owned())
                .collect();
            assert_eq!(actual, expected);
            for family in NativeRecordFamily::all() {
                assert_eq!(NativeRecordFamily::from_id(family.id()), Some(family));
                if matches!(family, NativeRecordFamily::TableOwners | NativeRecordFamily::GraphLookups | NativeRecordFamily::OccurrenceSkips | NativeRecordFamily::OccurrenceBlockMax | NativeRecordFamily::OccurrenceGuards) { continue; }
                let layout = family.layout();
                let mut statement = connection.prepare(&format!("PRAGMA table_info({})", layout.table))?;
                let columns = statement.query_map([], |row| {
                    Ok((row.get::<_, String>(1)?, row.get::<_, u16>(5)?, row.get::<_, String>(2)?, row.get::<_, bool>(3)?))
                })?.collect::<Result<Vec<_>, _>>()?;
                assert_eq!(columns.iter().map(|(name, ..)| name.as_str()).collect::<Vec<_>>(), layout.columns, "{}", layout.table);
                let mut keys: Vec<_> = columns.iter().enumerate().filter(|(_, (_, position, ..))| *position != 0).map(|(slot, (_, position, ..))| (*position, slot)).collect();
                keys.sort_unstable();
                assert_eq!(keys.iter().map(|(_, slot)| *slot).collect::<Vec<_>>(), layout.primary_key, "{}", layout.table);
                assert!(layout.identity_columns.iter().all(|column| layout.primary_key.contains(column)));
                assert_eq!(layout.columns.len(), layout.column_types.len());
                assert_eq!(layout.columns.len(), layout.nullable.len());
                for (slot, (_, _, kind, not_null)) in columns.iter().enumerate() {
                    assert_eq!(layout.column_types[slot].declaration(), kind, "{}", layout.table);
                    assert_eq!(layout.nullable[slot], !not_null, "{}", layout.table);
                }
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(NativeRecordFamily::from_id(0), None);
    assert_eq!(NativeRecordFamily::from_id(u16::MAX), None);
}

#[test]
fn document_identity_uses_the_owner_generation_and_survives_a_relation_rename() {
    let control = StorageReadControl::with_limit(1 << 20);
    let row = [
        ValueRef::Text(b"public.old"),
        ValueRef::Integer(17),
        ValueRef::Text(b"{}"),
        ValueRef::Null,
    ];
    let original =
        NativeRecord::encode(NativeRecordFamily::Documents, owner(), &row, &control).unwrap();
    let mut renamed = row;
    renamed[0] = ValueRef::Text(b"other.renamed");
    let renamed =
        NativeRecord::encode(NativeRecordFamily::Documents, owner(), &renamed, &control).unwrap();
    assert_eq!(original.key(), renamed.key());
    assert_ne!(original.row(), renamed.row());
    for changed in [
        NativeRecordOwner::Object {
            identity: [4; 16],
            generation: [7; 16],
        },
        NativeRecordOwner::Object {
            identity: [3; 16],
            generation: [8; 16],
        },
    ] {
        let replacement =
            NativeRecord::encode(NativeRecordFamily::Documents, changed, &row, &control).unwrap();
        assert_ne!(original.key(), replacement.key());
    }
    let missing = NativeRecordOwner::Object {
        identity: [0; 16],
        generation: [1; 16],
    };
    assert!(NativeRecordIdentity::new(NativeRecordFamily::Documents, missing).is_err());
    let database = NativeRecordOwner::Database(DatabaseId::from_bytes([1; 16]));
    assert!(NativeRecordIdentity::new(NativeRecordFamily::Documents, database).is_err());
    assert!(NativeRecordIdentity::new(NativeRecordFamily::Metadata, owner()).is_err());
}

#[test]
fn native_primary_keys_preserve_signed_integer_and_binary_component_order() {
    let control = StorageReadControl::with_limit(1 << 20);
    let integer = NativeRecordIdentity::new(NativeRecordFamily::Documents, owner()).unwrap();
    let values = [i64::MIN, -65_536, -1, 0, 1, 65_536, i64::MAX];
    let keys: Vec<_> = values
        .iter()
        .map(|value| {
            integer
                .encode_key(&[ValueRef::Integer(*value)], &control)
                .unwrap()
        })
        .collect();
    assert!(keys.windows(2).all(|pair| pair[0][..] < pair[1][..]));
    let binary =
        NativeRecordIdentity::new(NativeRecordFamily::OccurrenceClusters, owner()).unwrap();
    let terms: &[&[u8]] = &[
        b"", b"\0", b"\0\0", b"\0a", b"a", b"a\0", b"a\0a", b"aa", b"\xff",
    ];
    let keys: Vec<_> = terms
        .iter()
        .map(|term| {
            binary
                .encode_key(
                    &[
                        ValueRef::Text(b"f\0ield"),
                        ValueRef::Blob(term),
                        ValueRef::Integer(0),
                    ],
                    &control,
                )
                .unwrap()
        })
        .collect();
    assert!(keys.windows(2).all(|pair| pair[0][..] < pair[1][..]));
    let prefix = binary
        .encode_prefix(&[ValueRef::Text(b"f\0ield")], &control)
        .unwrap();
    assert!(keys.iter().all(|key| key.starts_with(&prefix)));
    let other = binary
        .encode_key(
            &[
                ValueRef::Text(b"f"),
                ValueRef::Blob(b"ield"),
                ValueRef::Integer(0),
            ],
            &control,
        )
        .unwrap();
    assert!(!other.starts_with(&prefix));
    for value in [ValueRef::Null, ValueRef::Real(1.0), ValueRef::Text(&[255])] {
        assert!(integer.encode_key(&[value], &control).is_err());
    }
    assert!(integer.encode_key(&[], &control).is_err());
    assert!(integer
        .encode_prefix(&[ValueRef::Integer(1), ValueRef::Integer(2)], &control)
        .is_err());
}

#[test]
fn native_rows_preserve_storage_classes_and_borrow_encoded_payloads() {
    let control = StorageReadControl::with_limit(1 << 20);
    let values = [
        ValueRef::Null,
        ValueRef::Integer(i64::MIN),
        ValueRef::Integer(i64::MAX),
        ValueRef::Real(-0.0),
        ValueRef::Real(f64::INFINITY),
        ValueRef::Text(b"a\0\xe6\x97\xa5"),
        ValueRef::Blob(b"a\0\xff"),
        ValueRef::Text(b""),
        ValueRef::Blob(b""),
    ];
    let encoded = encode_row(&values, &control).unwrap();
    let decoded = decode_row(&encoded, values.len(), &control).unwrap();
    assert_eq!(&*decoded, &values);
    let ValueRef::Real(value) = decoded[3] else {
        panic!("REAL storage class lost")
    };
    assert_eq!(value.to_bits(), (-0.0_f64).to_bits());
    for value in &*decoded {
        if let ValueRef::Text(bytes) | ValueRef::Blob(bytes) = value {
            let start = bytes.as_ptr() as usize;
            assert!(start >= encoded.as_ptr() as usize);
            assert!(start + bytes.len() <= encoded.as_ptr() as usize + encoded.len());
        }
    }
    assert!(encode_row(&[ValueRef::Real(f64::NAN)], &control).is_err());
    assert!(encode_row(&[ValueRef::Text(&[255])], &control).is_err());
}

#[test]
fn native_rows_reject_corruption_and_release_failed_allocation_reservations() {
    let control = StorageReadControl::with_limit(1 << 20);
    let original = encode_row(&[ValueRef::Text(b"row"), ValueRef::Integer(3)], &control).unwrap();
    for length in 0..original.len() {
        assert!(decode_row(&original[..length], 2, &control).is_err());
    }
    let mut trailing = original.to_vec();
    trailing.push(0);
    assert!(decode_row(&trailing, 2, &control).is_err());
    assert!(decode_row(&original, 1, &control).is_err());
    let mut unknown = original.to_vec();
    unknown[6] = 255;
    assert!(decode_row(&unknown, 2, &control).is_err());
    let small = StorageReadControl::with_limit(64);
    assert!(matches!(
        encode_row(&[ValueRef::Blob(&[0; 256])], &small),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(small.memory().used(), 0);
    let tiny = StorageReadControl::with_limit(1);
    assert!(matches!(
        decode_row(&original, 2, &tiny),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(tiny.memory().used(), 0);
}

#[test]
fn native_record_fixture_pins_family_and_owner_encoding_and_checks_the_row_key() {
    let control = StorageReadControl::with_limit(4096);
    let row = [
        ValueRef::Text(b"t"),
        ValueRef::Integer(17),
        ValueRef::Text(b"{}"),
        ValueRef::Null,
    ];
    let record =
        NativeRecord::encode(NativeRecordFamily::Documents, owner(), &row, &control).unwrap();
    let mut expected = b"\0uqa-native-record\x01\0\x0a\x01".to_vec();
    expected.extend_from_slice(&[3; 16]);
    expected.extend_from_slice(&[7; 16]);
    expected.extend_from_slice(b"\x01\x80\0\0\0\0\0\0\x11");
    assert_eq!(record.key(), expected);
    let (identity, decoded) = decode_record(record.key(), record.row(), &control).unwrap();
    assert_eq!(identity.owner(), owner());
    assert_eq!(identity.family(), NativeRecordFamily::Documents);
    assert_eq!(&*decoded, &row);
    for length in 0..record.key().len() {
        assert!(decode_record(&record.key()[..length], record.row(), &control).is_err());
    }
    let mut wrong_key = record.key().to_vec();
    *wrong_key.last_mut().unwrap() += 1;
    assert!(decode_record(&wrong_key, record.row(), &control).is_err());
    let mut trailing_key = record.key().to_vec();
    trailing_key.push(0);
    assert!(decode_record(&trailing_key, record.row(), &control).is_err());
    let mut wrong_type = row;
    wrong_type[1] = ValueRef::Text(b"17");
    assert!(NativeRecord::encode(
        NativeRecordFamily::Documents,
        owner(),
        &wrong_type,
        &control
    )
    .is_err());
    wrong_type[1] = ValueRef::Null;
    assert!(NativeRecord::encode(
        NativeRecordFamily::Documents,
        owner(),
        &wrong_type,
        &control
    )
    .is_err());
}

#[test]
fn native_record_encoding_honors_cancellation_without_retaining_buffers() {
    let control = StorageReadControl::with_limit(4096);
    let row = [
        ValueRef::Text(b"t"),
        ValueRef::Integer(17),
        ValueRef::Text(b"{}"),
        ValueRef::Null,
    ];
    let record =
        NativeRecord::encode(NativeRecordFamily::Documents, owner(), &row, &control).unwrap();
    let used = control.memory().used();
    control.cancellation().cancel();
    assert!(matches!(
        NativeRecord::encode(NativeRecordFamily::Documents, owner(), &row, &control),
        Err(VersionError::Cancelled(_))
    ));
    assert!(matches!(
        decode_record(record.key(), record.row(), &control),
        Err(VersionError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), used);
    drop(record);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_definition_records_validate_the_persisted_object_identity_and_generation() {
    let control = StorageReadControl::with_limit(1 << 20);
    for (family, generation) in [
        (NativeRecordFamily::Tables, "storage_generation"),
        (NativeRecordFamily::Sequences, "definition_generation"),
    ] {
        let layout = family.layout();
        let mut values: Vec<_> = layout
            .column_types
            .iter()
            .map(|kind| match kind {
                NativeColumnType::Integer => ValueRef::Integer(1),
                NativeColumnType::Real => ValueRef::Real(1.0),
                NativeColumnType::Text | NativeColumnType::TextOrBlob => {
                    ValueRef::Text(b"original")
                }
                NativeColumnType::Blob => ValueRef::Blob(&[9]),
            })
            .collect();
        let identity_column = layout
            .columns
            .iter()
            .position(|column| *column == "object_id")
            .unwrap();
        let generation_column = layout
            .columns
            .iter()
            .position(|column| *column == generation)
            .unwrap();
        values[identity_column] = ValueRef::Blob(&[3; 16]);
        values[generation_column] = ValueRef::Blob(&[7; 16]);
        let original = NativeRecord::encode(family, owner(), &values, &control).unwrap();
        values[0] = ValueRef::Text(b"renamed_schema");
        values[1] = ValueRef::Text(b"renamed_relation");
        let renamed = NativeRecord::encode(family, owner(), &values, &control).unwrap();
        assert_eq!(original.key(), renamed.key());
        decode_record(renamed.key(), renamed.row(), &control).unwrap();
        for column in [identity_column, generation_column] {
            let expected = values[column];
            values[column] = ValueRef::Blob(&[8; 16]);
            assert!(NativeRecord::encode(family, owner(), &values, &control).is_err());
            let wrong_row = encode_row(&values, &control).unwrap();
            assert!(decode_record(original.key(), &wrong_row, &control).is_err());
            values[column] = expected;
        }
    }
}
