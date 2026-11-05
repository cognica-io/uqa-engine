//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeMap;

fn address(object_id: u32) -> CommentAddress {
    CommentAddress {
        object_id,
        sub_id: 0,
        class_id: 1247,
    }
}

#[test]
fn native_comment_capture_shares_exact_roots_without_visiting_payloads() {
    for count in [1, 64, 4096] {
        let native = MemoryBudget::new(8 << 20);
        let mut source = CommentRegistry::with_budget(&native);
        for index in 0..count {
            source
                .insert(
                    address(index + 1),
                    "original 한글\n",
                    RelationPersistence::Permanent,
                )
                .unwrap();
        }
        let memory = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut captured = None;
        let allocation = allocation_counter::measure(|| {
            captured = Some(source.capture(&control).unwrap());
        });
        assert_eq!(allocation.count_total, 0);
        let captured = captured.unwrap();
        assert_eq!(captured.len(), usize::try_from(count).unwrap());
        assert!(std::ptr::eq(
            source.get(&address(1)).unwrap(),
            captured.get(&address(1)).unwrap(),
        ));
        let required = memory.used();
        let denied = MemoryBudget::new(required - 1);
        assert_eq!(
            source
                .capture(&ProductionControl::new(&denied, &token, &token))
                .unwrap_err()
                .sqlstate(),
            Some("53200"),
        );
        assert_eq!(denied.used(), 0);
        let exact = MemoryBudget::new(required);
        let independent = source
            .capture(&ProductionControl::new(&exact, &token, &token))
            .unwrap();
        source
            .insert(address(1), "replacement", RelationPersistence::Permanent)
            .unwrap();
        drop(source);
        assert!(captured.iter().all(|(_, text)| text == "original 한글\n"));
        drop(captured);
        assert_eq!(memory.used(), 0);
        assert!(native.used() > 0);
        drop(independent);
        assert_eq!(exact.used(), 0);
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn native_comment_changes_share_unrelated_text_and_preserve_failed_roots() {
    let memory = MemoryBudget::new(16 << 20);
    let mut source = CommentRegistry::with_budget(&memory);
    for index in 1..=128 {
        source
            .insert(address(index), "original", RelationPersistence::Permanent)
            .unwrap();
    }
    source
        .insert(
            address(129),
            &"x".repeat(4 << 20),
            RelationPersistence::Permanent,
        )
        .unwrap();
    let original = source.clone();
    let allocated = allocation_counter::measure(|| {
        source
            .insert(address(64), "replacement", RelationPersistence::Permanent)
            .unwrap();
        source.remove(&address(1)).unwrap();
    });
    assert!(allocated.bytes_total < 16 << 10, "{allocated:?}");
    assert!(allocated.count_total < 128, "{allocated:?}");
    assert!(std::ptr::eq(
        source.get(&address(129)).unwrap(),
        original.get(&address(129)).unwrap()
    ));
    assert_eq!(original[&address(64)], "original");
    assert_eq!(source[&address(64)], "replacement");
    let before = source.clone();
    let used = memory.used();
    let occupied = memory.reserve(memory.available()).unwrap();
    assert_eq!(
        source
            .insert(address(64), "denied", RelationPersistence::Permanent)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(
        source.remove(&address(2)).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert!(!source.remove(&address(0)).unwrap());
    assert_eq!(source, before);
    drop(occupied);
    assert_eq!(memory.used(), used);
    drop((source, original, before));
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_comment_ranges_preserve_object_subobject_and_class_order() {
    let mut values = BTreeMap::new();
    for object_id in 1..64 {
        for sub_id in [0, 1, 17] {
            for class_id in [1247, 1259, 9999] {
                values.insert(
                    CommentAddress {
                        object_id,
                        sub_id,
                        class_id,
                    },
                    format!("{object_id}/{sub_id}/{class_id}"),
                );
            }
        }
    }
    let source =
        CommentRegistry::try_from_iter(values.clone().into_iter().map(|(address, text)| {
            let persistence = if address.object_id % 2 == 0 {
                RelationPersistence::Temporary
            } else {
                RelationPersistence::Permanent
            };
            (address, text, persistence)
        }))
        .unwrap();
    for (start, end) in [(0, 0), (0, 3), (16, 16), (32, 49), (63, 65)] {
        let first = CommentAddress {
            object_id: start,
            sub_id: 0,
            class_id: 0,
        };
        let last = CommentAddress {
            object_id: end,
            sub_id: i32::MAX,
            class_id: u32::MAX,
        };
        assert!(source.range(first..=last).eq(values.range(first..=last)));
    }
    let memory = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    for original_cancelled in [true, false] {
        let token = CancellationToken::new();
        let control = if original_cancelled {
            original.cancel();
            ProductionControl::new(&memory, &original, &token)
        } else {
            invoking.cancel();
            ProductionControl::new(&memory, &token, &invoking)
        };
        assert!(matches!(
            source.capture(&control),
            Err(SQLError::Cancelled(_))
        ));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_comment_scope_changes_publish_complete_roots_or_preserve_original_eligibility() {
    for slack in [0, 64, 128, 256, 512, 1024, 4096] {
        for destination in [
            RelationPersistence::Permanent,
            RelationPersistence::Temporary,
        ] {
            let memory = MemoryBudget::new(1 << 20);
            let mut source = CommentRegistry::with_budget(&memory);
            for index in 1..64 {
                source
                    .insert(
                        address(index),
                        "original",
                        if index % 2 == 0 {
                            RelationPersistence::Temporary
                        } else {
                            RelationPersistence::Permanent
                        },
                    )
                    .unwrap();
            }
            let selected = if destination == RelationPersistence::Temporary {
                31
            } else {
                32
            };
            let original = source.clone();
            let before = memory.used();
            let occupied = memory.reserve(memory.available() - slack).unwrap();
            match source.insert(address(selected), "changed", destination) {
                Ok(()) => {
                    assert_eq!(source.len(), original.len());
                    assert_eq!(source[&address(selected)], "changed");
                    assert_eq!(
                        source.is_temporary(&address(selected)),
                        destination == RelationPersistence::Temporary
                    );
                }
                Err(error) => {
                    assert_eq!(error.sqlstate(), Some("53200"));
                    assert_eq!(source, original);
                    for (key, text) in original.iter() {
                        assert!(std::ptr::eq(source.get(key).unwrap(), text));
                    }
                }
            }
            drop(occupied);
            if source == original {
                assert_eq!(memory.used(), before);
            }
            assert_eq!(original[&address(selected)], "original");
            source
                .insert(address(selected), "usable", destination)
                .unwrap();
            let durable = source.durable_generation();
            assert_eq!(
                durable.get(&address(selected)).is_some(),
                destination != RelationPersistence::Temporary
            );
            assert_eq!(source.iter().count(), source.len());
            drop((source, original, durable));
            assert_eq!(memory.used(), 0);
        }
    }
}
