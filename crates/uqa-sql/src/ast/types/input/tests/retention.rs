//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_type_input_cancels_during_original_name_production() {
    let text = format!(r#"{{"Named":"{}"}}"#, "x".repeat(16 << 20));
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
            4096,
            || {
                let control = ProductionControl::new(&memory, &original, &invoking);
                ColumnType::from_stored_json_with_control(&text, &control)
            },
        );
        assert_eq!(output.unwrap_err().sqlstate(), Some("57014"));
        assert!(memory.peak() >= 4096);
        assert_eq!(memory.used(), 0);
    }
}

fn retained(text: &str, limit: usize) -> (Result<Produced<ColumnType>, SQLError>, MemoryBudget) {
    let memory = MemoryBudget::new(limit);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    (
        ColumnType::from_stored_json_with_control(text, &control),
        memory,
    )
}

#[test]
fn stored_type_output_retains_exact_live_allocations_and_admits_before_copy() {
    for &(tag, payload, _) in STRUCTS {
        let text = tagged("Array", &tagged(tag, payload));
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output = Some(ColumnType::from_stored_json_with_control(&text, &control).unwrap());
        });
        assert_eq!(
            usize::try_from(allocations.bytes_current).unwrap(),
            memory.used()
        );
        assert_eq!(memory.used(), output.as_ref().unwrap().reserved_bytes());
        let peak = memory.peak();
        drop(output);
        assert_eq!(memory.used(), 0);
        let (exact, exact_memory) = retained(&text, peak);
        assert!(exact.is_ok());
        assert_eq!(exact_memory.peak(), peak);
        drop(exact);
        assert_eq!(exact_memory.used(), 0);
        let (denied, denied_memory) = retained(&text, peak - 1);
        assert_eq!(denied.unwrap_err().sqlstate(), Some("53200"));
        assert_eq!(denied_memory.used(), 0);
    }
    let text = tagged(
        "Named",
        &serde_json::to_string(&"x".repeat(1 << 20)).unwrap(),
    );
    let allocations = allocation_counter::measure(|| {
        let (value, memory) = retained(&text, 4096);
        assert_eq!(value.unwrap_err().sqlstate(), Some("53200"));
        assert_eq!(memory.used(), 0);
    });
    assert!(allocations.bytes_max < 8192, "{allocations:?}");
}

#[test]
fn stored_type_failures_and_both_cancellation_owners_retire_partial_payloads() {
    for &(tag, payload, _) in STRUCTS {
        let text = tagged(tag, payload);
        for limit in [0, 1, 8, 16, 32, 64, 128, 256] {
            let (value, memory) = retained(&text, limit);
            drop(value);
            assert_eq!(memory.used(), 0);
        }
        for original_cancelled in [false, true] {
            let memory = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if original_cancelled {
                original.cancel();
            } else {
                invoking.cancel();
            }
            let control = ProductionControl::new(&memory, &original, &invoking);
            let value = ColumnType::from_stored_json_with_control(&text, &control);
            assert_eq!(value.unwrap_err().sqlstate(), Some("57014"));
            assert_eq!(memory.used(), 0);
        }
    }
    let text = r#"{"Domain":{"schema":"long schema","name":"long name","oid":42,"base":{"Array":"Text"},"oid":42}}"#;
    let (value, memory) = retained(text, 1 << 20);
    assert!(value.is_err());
    assert!(memory.peak() > 0);
    assert_eq!(memory.used(), 0);
}
