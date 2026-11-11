//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_table_constraints_retain_allocations_and_retire_every_partial_family() {
    let text = fixture().to_string();
    let memory = MemoryBudget::new(1 << 24);
    let token = CancellationToken::new();
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(read_stored_table_constraints_json(&text, &memory, &token, &token).unwrap());
    });
    assert_eq!(
        usize::try_from(allocations.bytes_current).unwrap(),
        memory.used()
    );
    assert!(
        usize::try_from(allocations.bytes_max).unwrap() <= memory.peak(),
        "{allocations:?}, peak {}",
        memory.peak()
    );
    assert_eq!(output.as_ref().unwrap().reserved_bytes(), memory.used());
    let peak = memory.peak();
    token.cancel();
    assert!(serde_json::to_string(&**output.as_ref().unwrap()).is_ok());
    drop(output);
    assert_eq!(memory.used(), 0);
    for limit in (0..peak).step_by(251).chain([peak - 1]) {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        assert!(
            read_stored_table_constraints_json(&text, &memory, &token, &token).is_err(),
            "limit {limit}/{peak}"
        );
        assert_eq!(memory.used(), 0);
        assert!(memory.peak() <= limit);
    }
}

#[test]
fn stored_table_constraints_check_both_owners_before_payload_allocation() {
    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            read_stored_table_constraints_json("{}", &memory, &original, &invoking)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.peak(), 0);
    }
    let text = format!(
        r#"{{"hierarchy":{{"parents":["{}"]}}}}"#,
        "x".repeat(2 << 20)
    );
    let memory = MemoryBudget::new(128);
    let token = CancellationToken::new();
    let allocations = allocation_counter::measure(|| {
        assert_eq!(
            read_stored_table_constraints_json(&text, &memory, &token, &token)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(allocations.bytes_total < 8192, "{allocations:?}");
    assert_eq!(memory.used(), 0);
    assert!(memory.peak() <= memory.limit());
}

#[test]
fn stored_table_constraints_cancel_active_parent_and_constraint_ownership() {
    let text = fixture()
        .to_string()
        .replace("s.one", &"x".repeat(16 << 20));
    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(64 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let cancel = if original_cancelled {
            &original
        } else {
            &invoking
        };
        let output = crate::ast::stored_input::test_control::cancel_after_allocation(
            &memory,
            cancel,
            65536,
            || read_stored_table_constraints_json(&text, &memory, &original, &invoking),
        );
        assert_eq!(output.unwrap_err().sqlstate(), Some("57014"));
        assert!(memory.peak() >= 65536);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_table_constraints_deep_partition_input_and_retirement_use_a_small_stack() {
    let expression = format!(
        r#"{}{{"Literal":true}}{}"#,
        r#"{"Not":"#.repeat(118),
        "}".repeat(118)
    );
    let text = format!(r#"{{"hierarchy":{{"partition_bound":{{"List":[{expression}]}}}}}}"#);
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(1 << 24);
            let token = CancellationToken::new();
            let table = read_stored_table_constraints_json(&text, &memory, &token, &token).unwrap();
            assert!(table.hierarchy.partition_bound.is_some());
            drop(table);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
