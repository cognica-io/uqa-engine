//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_columns_json_retains_native_allocations_and_retires_partial_admission() {
    let text = complete().to_string();
    let memory = MemoryBudget::new(1 << 24);
    let token = CancellationToken::new();
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(read_stored_columns_json(&text, &memory, &token, &token).unwrap());
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
    for limit in (0..peak).step_by(173).chain([peak - 1]) {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        assert!(
            read_stored_columns_json(&text, &memory, &token, &token).is_err(),
            "limit {limit}/{peak}"
        );
        assert_eq!(memory.used(), 0);
        assert!(memory.peak() <= limit);
    }
}

#[test]
fn stored_columns_json_checks_both_owners_and_denies_payload_before_allocation() {
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
            read_stored_columns_json(SIMPLE, &memory, &original, &invoking)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.peak(), 0);
    }
    let text = SIMPLE.replace("column", &"x".repeat(2 << 20));
    let memory = MemoryBudget::new(128);
    let token = CancellationToken::new();
    let allocations = allocation_counter::measure(|| {
        assert_eq!(
            read_stored_columns_json(&text, &memory, &token, &token)
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
fn stored_columns_json_cancellation_retires_active_declarations() {
    let text = complete()
        .to_string()
        .replace("한글_column", &"x".repeat(16 << 20));
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
            || read_stored_columns_json(&text, &memory, &original, &invoking),
        );
        assert_eq!(output.unwrap_err().sqlstate(), Some("57014"));
        assert!(memory.peak() >= 65536);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_columns_json_deep_expression_input_and_retirement_use_a_small_stack() {
    let expression = format!(
        r#"{}{{"Literal":true}}{}"#,
        r#"{"Not":"#.repeat(122),
        "}".repeat(122)
    );
    let text = SIMPLE.replace(
        r#""name":"column""#,
        &format!(r#""default":{expression},"name":"column""#),
    );
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(1 << 24);
            let token = CancellationToken::new();
            let columns = read_stored_columns_json(&text, &memory, &token, &token).unwrap();
            assert!(matches!(columns[0].default, Some(crate::ast::Expr::Not(_))));
            drop(columns);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
