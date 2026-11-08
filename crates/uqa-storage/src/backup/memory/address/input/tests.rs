//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::backup::{MemoryImageKey, MemoryTableUnit, NamespaceId};
use uqa_core::RelationIdentity;

fn address(family: u16, key: &[u8]) -> UnitAddress<'_> {
    UnitAddress {
        namespace: memory_image_namespace(),
        generation: 0,
        family,
        key,
    }
}

fn table_key(suffix: &[u8]) -> Vec<u8> {
    let mut key = b"s\0\xffx\0\0\xc3\xa9\0\0".to_vec();
    key.extend(0..16);
    key.extend(240..=255);
    key.extend_from_slice(suffix);
    key
}

#[test]
fn memory_address_decode_preserves_independent_wire_identities() {
    let control = StorageReadControl::with_limit(4096);
    assert!(matches!(
        read_memory_image_address(address(1, &[]), &control).unwrap(),
        DecodedMemoryImageAddress::Header
    ));
    assert!(matches!(
        read_memory_image_address(address(2, &[0x12, 0x34]), &control).unwrap(),
        DecodedMemoryImageAddress::Catalog(0x1234)
    ));
    let DecodedMemoryImageAddress::FieldAnalyzer { table, field } =
        read_memory_image_address(address(3, b"a\0\xffb\0\0\xc3\xa9\0\0"), &control).unwrap()
    else {
        panic!("field binding address")
    };
    assert_eq!(&*table, "a\0b");
    assert_eq!(&*field, "é");
    drop((table, field));
    let DecodedMemoryImageAddress::Resource { kind, name } =
        read_memory_image_address(address(4, b"\x04\0\0"), &control).unwrap()
    else {
        panic!("resource address")
    };
    assert_eq!(kind, 4);
    assert_eq!(&*name, "");
    drop(name);

    let suffixes: &[&[u8]] = &[
        &[0, 9],
        &[1, 0, 0, 0, 0, 0, 0, 0, 0],
        &[2, 255, 255, 255, 255, 255, 255, 255, 255],
        b"\x03v\0\xffx\0\0",
        b"\x04idx\0\0\0",
        b"\x04idx\0\0\x01\x12\x34\x56\x78\x9a\xbc\xde\xf0",
    ];
    for (index, suffix) in suffixes.iter().enumerate() {
        let key = table_key(suffix);
        let DecodedMemoryImageAddress::Table {
            schema,
            relation,
            object,
            generation,
            unit,
        } = read_memory_image_address(address(5, &key), &control).unwrap()
        else {
            panic!("table address")
        };
        assert_eq!(&*schema, "s\0x");
        assert_eq!(&*relation, "é");
        assert_eq!(object, std::array::from_fn(|i| i as u8));
        assert_eq!(generation, std::array::from_fn(|i| 240 + i as u8));
        let component = match (index, &unit) {
            (0, DecodedMemoryTableUnit::Metadata(9)) => MemoryTableUnit::Metadata(9),
            (1, DecodedMemoryTableUnit::Document(0)) => MemoryTableUnit::Document(0),
            (2, DecodedMemoryTableUnit::Text(u64::MAX)) => MemoryTableUnit::Text(u64::MAX),
            (3, DecodedMemoryTableUnit::Vector(field)) => {
                assert_eq!(&**field, "v\0x");
                MemoryTableUnit::Vector(field)
            }
            (4, DecodedMemoryTableUnit::ValueIndex { name, document }) => {
                assert_eq!(&**name, "idx");
                assert_eq!(*document, None);
                MemoryTableUnit::ValueIndex {
                    name,
                    document: *document,
                }
            }
            (5, DecodedMemoryTableUnit::ValueIndex { name, document }) => {
                assert_eq!(&**name, "idx");
                assert_eq!(*document, Some(0x1234_5678_9abc_def0));
                MemoryTableUnit::ValueIndex {
                    name,
                    document: *document,
                }
            }
            _ => panic!("incorrect table component {unit:?}"),
        };
        let encoded = MemoryImageKey::table(
            &RelationIdentity::new(&*schema, &*relation),
            object,
            generation,
            component,
            &control,
        )
        .unwrap()
        .into_bytes();
        assert_eq!(&*encoded, &key);
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_address_decode_rejects_malformed_envelopes_and_names() {
    let control = StorageReadControl::with_limit(4096);
    let mut wrong = address(1, &[]);
    wrong.namespace = NamespaceId::from_bytes([9; 16]).unwrap();
    assert!(read_memory_image_address(wrong, &control).is_err());
    wrong = address(1, &[]);
    wrong.generation = 1;
    assert!(read_memory_image_address(wrong, &control).is_err());
    for family in [0, 6, u16::MAX] {
        assert!(read_memory_image_address(address(family, &[]), &control).is_err());
    }
    for (family, key) in [(1, &[0][..]), (2, &[0][..]), (2, &[0, 1, 2][..])] {
        assert!(read_memory_image_address(address(family, key), &control).is_err());
    }
    for bytes in [
        &b"a\0\0b\0"[..],
        &b"a\0\0b\0\0\0"[..],
        &b"a\0\0\xff\0\0"[..],
        &b"a\0\0\xc0\x80\0\0"[..],
        &b"a\0\0\xed\xa0\x80\0\0"[..],
        &b"a\0\0\xc3\0\0"[..],
    ] {
        assert!(read_memory_image_address(address(3, bytes), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    for tag in 1..255 {
        let bytes = [b'a', 0, tag, 0, 0, 0, 0];
        assert!(read_memory_image_address(address(3, &bytes), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn memory_table_address_decode_rejects_noncanonical_components() {
    let control = StorageReadControl::with_limit(4096);
    let key = table_key(b"\x04idx\0\0\x01\x12\x34\x56\x78\x9a\xbc\xde\xf0");
    for end in 0..key.len() {
        assert!(read_memory_image_address(address(5, &key[..end]), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    for suffix in [
        &b"\x05"[..],
        &b"\x00\x09\x00"[..],
        &b"\x03v\0\x01"[..],
        &b"\x04idx\0\0\x02"[..],
        &b"\x04idx\0\0\x00\x00"[..],
    ] {
        assert!(read_memory_image_address(address(5, &table_key(suffix)), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn memory_address_decode_owns_admitted_names_and_releases_partial_input() {
    let control = StorageReadControl::with_limit(32 * 1024);
    let mut key = vec![4];
    for _ in 0..2048 {
        key.extend_from_slice(b"\xc3\xa9\0\xff");
    }
    key.extend_from_slice(&[0, 0]);
    let DecodedMemoryImageAddress::Resource { kind, name } =
        read_memory_image_address(address(4, &key), &control).unwrap()
    else {
        panic!("resource address")
    };
    drop(key);
    assert_eq!(kind, 4);
    assert_eq!(&*name, "é\0".repeat(2048));
    assert_eq!(control.memory().used(), name.capacity());
    drop(name);
    assert_eq!(control.memory().used(), 0);

    let small = StorageReadControl::with_limit(16);
    assert!(
        read_memory_image_address(address(3, b"first\0\0second-name-too-large\0\0"), &small)
            .is_err()
    );
    assert_eq!(small.memory().used(), 0);
    let cancelled = StorageReadControl::with_limit(4096);
    cancelled.cancellation().cancel();
    assert!(read_memory_image_address(address(1, &[]), &cancelled).is_err());
    assert!(read_memory_image_address(address(5, &table_key(&[0, 0])), &cancelled).is_err());
    assert_eq!(cancelled.memory().used(), 0);
}
