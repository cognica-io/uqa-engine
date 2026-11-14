//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::Produced;

fn admitted_key(text: &str, memory: &MemoryBudget) -> Budgeted<String> {
    let mut key = crate::memory::BudgetedString::new(memory);
    key.push_str(text).unwrap();
    let (key, memory) = key.into_parts();
    Budgeted::new(key, memory)
}

#[test]
fn consuming_map_transfer_preserves_original_key_allocations_and_releases_only_tree_nodes() {
    let memory = MemoryBudget::new(1 << 20);
    let mut map = OwnedMap::new();
    for id in [6, 2, 8, 1, 4, 7, 9, 0, 3, 5] {
        let text = format!("{id}{}", "가".repeat(129));
        map.insert_admitted_key(admitted_key(&text, &memory), id)
            .unwrap();
    }
    let expected = map
        .iter()
        .map(|(key, value)| (key.as_ptr(), key.capacity(), *value))
        .collect::<Vec<_>>();
    let bytes = memory.used();
    let hold = memory.reserve(memory.available()).unwrap();
    let mut values = Vec::with_capacity(map.len());
    let mut input = map.into_iter();
    let allocation = allocation_counter::measure(|| {
        for (index, &(pointer, capacity, value)) in expected.iter().enumerate() {
            let (key, actual) = input.next_retained().unwrap();
            assert_eq!(
                (key.as_ptr(), key.capacity(), actual),
                (pointer, capacity, value)
            );
            assert_eq!(key.reserved_bytes(), capacity);
            assert!(key.is_retained_by(&memory));
            assert_eq!(
                memory.used(),
                hold.bytes() + bytes - (index + 1) * OwnedMap::<String, i32>::entry_bytes()
            );
            values.push(key);
        }
        assert!(input.next_retained().is_none());
        assert_eq!(input.len(), 0);
    });
    assert_eq!(allocation.count_total, 0);
    drop((input, hold));
    assert_eq!(
        memory.used(),
        expected
            .iter()
            .map(|(_, capacity, _)| capacity)
            .sum::<usize>()
    );
    drop(values);
    assert_eq!(memory.used(), 0);
}

#[test]
fn consuming_map_transfer_retires_pending_nodes_without_releasing_yielded_keys() {
    for consumed in 0..=12 {
        let memory = MemoryBudget::new(1 << 20);
        let sibling = memory.reserve(7).unwrap();
        let mut map = OwnedMap::new();
        for id in 0..12 {
            map.insert_admitted_key(admitted_key(&format!("k{id:02}"), &memory), id)
                .unwrap();
        }
        let mut input = map.into_iter();
        let mut output = Vec::new();
        for _ in 0..consumed {
            output.push(input.next_retained().unwrap().0);
        }
        drop(input);
        assert_eq!(
            memory.used(),
            7 + output.iter().map(Produced::reserved_bytes).sum::<usize>()
        );
        drop(output);
        assert_eq!(memory.used(), 7);
        drop(sibling);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn consuming_map_transfer_keeps_ordinary_keys_uncontrolled_and_values_move_only() {
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    struct Key(u32);
    struct Value(String);
    let memory = MemoryBudget::new(1024);
    let mut map = OwnedMap::new();
    map.insert(Key(2), Value("ordinary".into()));
    map.insert_budgeted(Key(1), Value("node admitted".into()), &memory)
        .unwrap();
    let pointers = map
        .iter()
        .map(|(_, value)| value.0.as_ptr())
        .collect::<Vec<_>>();
    let mut input = map.into_iter();
    let (key, value) = input.next_retained().unwrap();
    assert_eq!(key.0, 1);
    assert_eq!(value.0.as_ptr(), pointers[0]);
    assert!(key.is_retained_by(&memory));
    assert_eq!(key.reserved_bytes(), 0);
    assert_eq!(memory.used(), 0);
    let (key, value) = input.next_retained().unwrap();
    assert_eq!(key.0, 2);
    assert_eq!(value.0.as_ptr(), pointers[1]);
    assert!(key.into_uncontrolled().is_ok());
    assert!(input.next_retained().is_none());
}
