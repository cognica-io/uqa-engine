//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::image::tests::byte_windows::{assert_retirement, read_windowed, WINDOWS};

fn encode(values: &ViewDefinitions) -> Vec<u8> {
    let memory = MemoryBudget::new(32 << 20);
    let result = bytes(&capture(values, &memory), 127);
    assert_eq!(memory.used(), 0);
    result
}

#[test]
fn native_view_input_preserves_original_fields_and_independent_mutations() {
    let (mut source, _) = fixture();
    let mut changed = source.get(&name(0)).unwrap().clone();
    changed
        .definition
        .options
        .push(("retained".into(), "한λ😀".repeat(1025)));
    changed.definition.materialized_rows[0]
        .insert("body".into(), Value::Str("원본🙂".repeat(1025)));
    source.insert(name(0), changed).unwrap();
    let expected = encode(&source);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(32 << 20);
        let mutations = MemoryBudget::new(32 << 20);
        let token = CancellationToken::new();
        let mut restored = read_windowed(
            ViewDefinitions::image_input(&memory, &mutations, &token).unwrap(),
            &expected,
            width,
            steps,
        );
        same(
            restored.get(&name(0)).unwrap(),
            source.get(&name(0)).unwrap(),
        );
        assert_eq!(encode(&restored), expected);
        let retained = restored.clone();
        let mut replacement = restored.get(&name(0)).unwrap().clone();
        replacement.definition.populated = false;
        let full = memory.reserve(memory.available()).unwrap();
        token.cancel();
        restored.insert(name(0), replacement).unwrap();
        assert!(!restored.get(&name(0)).unwrap().populated);
        assert_eq!(encode(&retained), expected);
        drop((restored, full));
        assert_eq!(mutations.used(), 0);
        assert!(memory.used() > 0);
        drop(retained);
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&expected, ViewDefinitions::image_input);
}

#[test]
fn native_view_input_keeps_identity_claims_across_completed_entries() {
    let (mut source, first) = fixture();
    let mut second = view(1);
    second.definition.object_id = source.get(&name(0)).unwrap().object_id;
    source.insert(name(1), second).unwrap();
    let expected = encode(&source);
    let memory = MemoryBudget::new(32 << 20);
    let token = CancellationToken::new();
    let mut input = ViewDefinitions::image_input(&memory, &memory, &token).unwrap();
    let mut reader = std::io::Cursor::new(expected);
    assert!(input.advance(&mut reader, 1).unwrap().is_none());
    assert_eq!(reader.position(), first.len() as u64);
    assert!(input.advance(&mut reader, 1).is_err());
    assert_eq!(memory.used(), 0);
    let position = reader.position();
    assert!(input.advance(&mut reader, 1).is_err());
    assert_eq!(reader.position(), position);
}

#[test]
fn native_view_input_retains_deep_queries_and_scalar_owners_on_small_stacks() {
    let (source, mut data) = fixture();
    let memory = MemoryBudget::new(32 << 20);
    let token = CancellationToken::new();
    let mut reader = source
        .get(&name(0))
        .unwrap()
        .query
        .image_reader(&memory, &token)
        .unwrap();
    let mut query_size = 0;
    let mut buffer = [0; 127];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        query_size += count;
    }
    drop(reader);
    data.truncate(data.len() - (query_size - 16));
    for level in 0..=10_000 {
        data.push(0);
        data.extend(0u64.to_le_bytes());
        data.push(2);
        data.extend(1u64.to_le_bytes());
        data.extend(1u64.to_le_bytes());
        if level == 10_000 {
            data.extend(std::iter::repeat_n(14, 10_000));
        }
        data.push(0);
        data.extend(u64::from(level < 10_000).to_le_bytes());
    }
    drop(source);
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(128 << 20);
            let mutations = MemoryBudget::new(16 << 20);
            let token = CancellationToken::new();
            let value = read_windowed(
                ViewDefinitions::image_input(&memory, &mutations, &token).unwrap(),
                &data,
                17,
                3,
            );
            let mut query = &value.get(&name(0)).unwrap().query;
            let mut depth = 0;
            while let RelationalPlan::Values { subqueries, .. } = &query.root {
                if subqueries.is_empty() {
                    break;
                }
                query = &subqueries[0];
                depth += 1;
            }
            assert_eq!(depth, 10_000);
            let full = memory.reserve(memory.available()).unwrap();
            assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
            drop(full);
            assert_eq!(memory.used(), 0);
            assert_eq!(mutations.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
