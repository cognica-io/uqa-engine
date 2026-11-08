//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn memory_address_names_preserve_native_order_without_delimiter_aliases() {
    let control = StorageReadControl::with_limit(1 << 20);
    let names = ["", "\0", "\0a", "a", "a\0", "aa", "z", "é", "한"];
    let mut previous = None;
    for table in names {
        for field in names {
            let next = MemoryImageKey::field_analyzer(table, field, &control)
                .unwrap()
                .into_bytes();
            assert!(previous
                .as_ref()
                .is_none_or(|key: &BudgetedVec<u8>| key[..] < next[..]));
            previous = Some(next);
        }
    }
    drop(previous);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_table_addresses_preserve_complete_generations_and_component_order() {
    let control = StorageReadControl::with_limit(1 << 20);
    let relation = RelationIdentity::new("a", "table");
    let units = [
        MemoryTableUnit::Metadata(0),
        MemoryTableUnit::Metadata(9),
        MemoryTableUnit::Document(0),
        MemoryTableUnit::Document(u64::MAX),
        MemoryTableUnit::Text(0),
        MemoryTableUnit::Text(u64::MAX),
        MemoryTableUnit::Vector(""),
        MemoryTableUnit::Vector("z"),
        MemoryTableUnit::ValueIndex {
            name: "a",
            document: None,
        },
        MemoryTableUnit::ValueIndex {
            name: "a",
            document: Some(0),
        },
        MemoryTableUnit::ValueIndex {
            name: "a",
            document: Some(u64::MAX),
        },
        MemoryTableUnit::ValueIndex {
            name: "b",
            document: None,
        },
    ];
    let mut previous = None;
    for unit in units {
        let next = MemoryImageKey::table(&relation, [1; 16], [2; 16], unit, &control)
            .unwrap()
            .into_bytes();
        assert!(previous
            .as_ref()
            .is_none_or(|key: &BudgetedVec<u8>| key[..] < next[..]));
        previous = Some(next);
    }
    drop(previous);
    let original = MemoryImageKey::table(
        &relation,
        [1; 16],
        [2; 16],
        MemoryTableUnit::Metadata(0),
        &control,
    )
    .unwrap()
    .into_bytes();
    for byte in 0..16 {
        let mut object = [1; 16];
        object[byte] = 3;
        let changed = MemoryImageKey::table(
            &relation,
            object,
            [2; 16],
            MemoryTableUnit::Metadata(0),
            &control,
        )
        .unwrap()
        .into_bytes();
        assert_ne!(&original[..], &changed[..]);
        let mut generation = [2; 16];
        generation[byte] = 3;
        let changed = MemoryImageKey::table(
            &relation,
            [1; 16],
            generation,
            MemoryTableUnit::Metadata(0),
            &control,
        )
        .unwrap()
        .into_bytes();
        assert_ne!(&original[..], &changed[..]);
    }
    drop(original);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_address_copy_obeys_memory_and_cancellation() {
    let small = StorageReadControl::with_limit(1);
    assert!(MemoryImageKey::resource(1, "resource", &small).is_err());
    assert_eq!(small.memory().used(), 0);
    let cancelled = StorageReadControl::with_limit(1024);
    cancelled.cancellation().cancel();
    assert!(MemoryImageKey::empty(&cancelled).is_err());
    assert_eq!(cancelled.memory().used(), 0);
}
