//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_index_definition_retains_native_allocations_and_retires_denied_input() {
    let text = serde_json::to_string(&fixture()).unwrap();
    let memory = MemoryBudget::new(1 << 24);
    let token = CancellationToken::new();
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(definition(Some(&text), &memory, &token, &token).unwrap());
    });
    assert_eq!(
        usize::try_from(allocations.bytes_current).unwrap(),
        memory.used()
    );
    assert!(
        usize::try_from(allocations.bytes_max).unwrap() <= memory.peak(),
        "{allocations:?}"
    );
    assert_eq!(output.as_ref().unwrap().reserved_bytes(), memory.used());
    let peak = memory.peak();
    token.cancel();
    assert_eq!(output.as_ref().unwrap().key_names, ["key", "expression"]);
    drop(output);
    assert_eq!(memory.used(), 0);
    for limit in (0..peak).step_by(97).chain([peak - 1]) {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        assert!(
            definition(Some(&text), &memory, &token, &token).is_err(),
            "{limit}/{peak}"
        );
        assert_eq!(memory.used(), 0);
        assert!(memory.peak() <= limit);
    }
}

#[test]
fn stored_index_definition_checks_both_owners_and_admits_before_payload_copy() {
    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        for text in [None, Some(r#"{"unique":false,"nulls_not_distinct":false}"#)] {
            assert_eq!(
                definition(text, &memory, &original, &invoking)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.peak(), 0);
        }
    }
    let mut native = fixture();
    native.catalog.as_mut().unwrap().physical_key = "x".repeat(2 << 20);
    let text = serde_json::to_string(&native).unwrap();
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let allocations = allocation_counter::measure(|| {
        assert_eq!(
            definition(Some(&text), &memory, &token, &token)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(allocations.bytes_total < 16384, "{allocations:?}");
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_index_definition_cancellation_retires_active_metadata_and_predicates() {
    for field in ["physical_key", "predicate"] {
        let mut native = fixture();
        if field == "physical_key" {
            native.catalog.as_mut().unwrap().physical_key = "x".repeat(1 << 20);
        } else {
            native.predicate = Some(Box::new(Expr::Literal(Value::Str("x".repeat(1 << 20)))));
        }
        let text = serde_json::to_string(&native).unwrap();
        for original_cancelled in [false, true] {
            let memory = MemoryBudget::new(8 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let cancel = if original_cancelled {
                &original
            } else {
                &invoking
            };
            let result = crate::ast::stored_input::test_control::cancel_after_allocation(
                &memory,
                cancel,
                65536,
                || definition(Some(&text), &memory, &original, &invoking),
            );
            assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn stored_index_definition_deep_predicate_input_and_retirement_use_a_small_stack() {
    let expression = format!(
        r#"{}{{"Literal":true}}{}"#,
        r#"{"Not":"#.repeat(124),
        "}".repeat(124)
    );
    let text = format!(r#"{{"predicate":{expression},"unique":false,"nulls_not_distinct":false}}"#);
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(1 << 24);
            let token = CancellationToken::new();
            let decoded = definition(Some(&text), &memory, &token, &token).unwrap();
            assert!(decoded.predicate.is_some());
            drop(decoded);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
