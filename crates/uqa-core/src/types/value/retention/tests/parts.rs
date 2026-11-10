//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn retained_payload_parts_preserve_independent_capacity_counts_without_copying() {
    let mut values = Vec::with_capacity(17);
    values.push(Value::Str(text(4096)));
    values.push(Value::Bytes(Vec::with_capacity(2048)));
    let expected = values.capacity() * size_of::<Value>() + 4096 + 2048;
    let value = Value::List(values);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut parts = value.retained_payload_parts_with_control(control);
    let mut total = 0;
    for part in parts.by_ref() {
        total += part.unwrap();
    }
    assert_eq!(total, expected);
    assert_eq!(memory.used(), 0);
    assert!(parts.next().is_none());
    assert_eq!(
        value.retained_payload_bytes_with_control(&control).unwrap(),
        expected
    );
    let scalar = Value::Str(text(8192));
    let allocations = allocation_counter::measure(|| {
        assert_eq!(
            scalar
                .retained_payload_parts_with_control(ProductionControl::uncontrolled())
                .map(Result::unwrap)
                .sum::<usize>(),
            8192
        );
    });
    assert_eq!(allocations.count_total, 0);
}

#[test]
fn retained_payload_parts_resume_deep_parent_retirement_without_recursive_or_unbounded_steps() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let depth = 4096;
            let mut value = Value::Null;
            for _ in 0..depth {
                value = Value::List(vec![value]);
            }
            let memory = MemoryBudget::new(4 << 20);
            let token = CancellationToken::new();
            let mut total = 0;
            let mut trailing_empty_steps = 0;
            for part in value.retained_payload_parts_with_control(ProductionControl::new(
                &memory, &token, &token,
            )) {
                let bytes = part.unwrap();
                total += bytes;
                if bytes == 0 {
                    trailing_empty_steps += 1;
                } else {
                    trailing_empty_steps = 0;
                }
            }
            assert_eq!(total, depth * size_of::<Value>());
            assert!(
                trailing_empty_steps >= depth,
                "returning through parents must retain separate work steps"
            );
            assert_eq!(memory.used(), 0);
            while let Value::List(mut children) = value {
                value = children.pop().unwrap();
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn retained_payload_parts_retire_original_scratch_on_denial_cancellation_and_drop() {
    let value = Value::List(vec![Value::Str(text(4096))]);
    for failure in ["original", "invoking", "memory", "drop"] {
        let memory = MemoryBudget::new(if failure == "memory" { 17 } else { 1 << 20 });
        let previous = memory.reserve(17).unwrap();
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut parts = value.retained_payload_parts_with_control(ProductionControl::new(
            &memory, &original, &invoking,
        ));
        if failure == "memory" {
            assert!(parts
                .by_ref()
                .any(|part| matches!(part, Err(ValueRetentionError::Memory(_)))));
        } else {
            while memory.used() == 17 {
                parts.next().unwrap().unwrap();
            }
            if failure == "drop" {
                drop(parts);
                assert_eq!(memory.used(), 17);
                continue;
            }
            if failure == "original" {
                original.cancel();
            } else {
                invoking.cancel();
            }
            assert!(matches!(
                parts.next(),
                Some(Err(ValueRetentionError::Cancelled(_)))
            ));
        }
        assert!(parts.next().is_none());
        assert_eq!(memory.used(), previous.bytes());
        drop((parts, previous));
        assert_eq!(memory.used(), 0);
    }
}
