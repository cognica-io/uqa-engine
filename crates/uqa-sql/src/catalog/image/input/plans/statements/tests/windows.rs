//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::image::input::window_tests::{assert_retirement, read_windowed, WINDOWS};

#[test]
fn native_statement_windows_preserve_original_query_and_mutation_fields() {
    for sql in QUERY_SOURCES.iter().chain(MUTATION_SOURCES) {
        let source = crate::compile(sql).unwrap().remove(0);
        let bytes = encode(&source);
        for (width, steps) in WINDOWS {
            let memory = MemoryBudget::new(64 << 20);
            let token = CancellationToken::new();
            let value = read_windowed(
                Statement::image_input(&memory, &token).unwrap(),
                &bytes,
                width,
                steps,
            );
            assert_eq!(
                serde_json::to_value(&*value).unwrap(),
                serde_json::to_value(&source).unwrap(),
                "{sql}"
            );
            assert_eq!(encode(&value), bytes);
            token.cancel();
            let full = memory.reserve(memory.available()).unwrap();
            assert_eq!(encode(&value), bytes);
            drop((value, full));
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn native_statement_windows_retire_partial_fields_and_original_parents() {
    for sql in [
        QUERY_SOURCES[7],
        QUERY_SOURCES[11],
        MUTATION_SOURCES[1],
        MUTATION_SOURCES[3],
    ] {
        assert_retirement(
            &encode(&crate::compile(sql).unwrap().remove(0)),
            Statement::image_input,
        );
    }
    let source = crate::compile(&format!("NOTIFY original, '{}'", "한λ😀".repeat(1025)))
        .unwrap()
        .remove(0);
    let bytes = encode(&source);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let value = read_windowed(
            Statement::image_input(&memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(encode(&value), bytes);
        drop(value);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_statement_windows_keep_deep_retirement_on_small_stacks() {
    let mut bytes = b"UQASTM01".to_vec();
    bytes.extend(1u64.to_le_bytes());
    bytes.push(1);
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(std::iter::repeat_n(14, 10_000));
    bytes.push(0);
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(64 << 20);
            let token = CancellationToken::new();
            let value = read_windowed(
                Statement::image_input(&memory, &token).unwrap(),
                &bytes,
                17,
                3,
            );
            assert_eq!(encode(&value), bytes);
            let full = memory.reserve(memory.available()).unwrap();
            assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
            drop(full);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
