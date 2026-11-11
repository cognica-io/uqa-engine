//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_call_input_cancels_during_original_argument_production() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Barrier,
    };

    let text = format!(
        r#"{{"name":"f","argument_types":["{}"]}}"#,
        "x".repeat(16 << 20)
    );
    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(64 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let done = AtomicBool::new(false);
        let ready = Barrier::new(2);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                ready.wait();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while memory.used() < 4096
                    && !done.load(Ordering::Acquire)
                    && std::time::Instant::now() < deadline
                {
                    std::thread::yield_now();
                }
                assert!(memory.used() >= 4096, "no argument allocation observed");
                assert!(
                    !done.load(Ordering::Acquire),
                    "production completed before observation"
                );
                if original_cancelled {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
            });
            ready.wait();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let output = FunctionBinding::from_stored_json_with_control(&text, &control);
            done.store(true, Ordering::Release);
            worker.join().unwrap();
            assert_eq!(output.unwrap_err().sqlstate(), Some("57014"));
        });
        assert!(memory.peak() >= 4096);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_call_optional_binding_rejection_has_no_unadmitted_diagnostic_allocation() {
    for field in [
        r#""assignment":null"#,
        r#""assignment":{"path":[],"value_argument":0.0}"#,
        r#""composite_field":{"type_oid":1,"number":1,"result_type":"unknown","dropped":false}"#,
    ] {
        let text = format!(r#"{{"name":"","argument_types":[],{field}}}"#);
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output = Some(FunctionBinding::from_stored_json_with_control(&text, &control).unwrap());
        });
        assert!(output.as_ref().unwrap().value_binding.is_none());
        assert_eq!(
            usize::try_from(allocations.bytes_current).unwrap(),
            memory.used()
        );
        assert!(
            usize::try_from(allocations.bytes_max).unwrap() <= memory.peak(),
            "{field}: {allocations:?}, peak {}",
            memory.peak()
        );
        drop(output);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_call_output_retains_exact_live_allocations_and_retires_every_denial() {
    for text in [COMPLETE.to_owned(), binding(ASSIGNMENT)] {
        let memory = MemoryBudget::new(1 << 24);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output = Some(FunctionBinding::from_stored_json_with_control(&text, &control).unwrap());
        });
        assert_eq!(
            usize::try_from(allocations.bytes_current).unwrap(),
            memory.used()
        );
        assert_eq!(output.as_ref().unwrap().reserved_bytes(), memory.used());
        let peak = memory.peak();
        token.cancel();
        assert!(!output.as_ref().unwrap().name.is_empty());
        drop(output);
        assert_eq!(memory.used(), 0);
        for limit in (0..peak).step_by(37).chain([peak - 1]) {
            let memory = MemoryBudget::new(limit);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            assert!(
                FunctionBinding::from_stored_json_with_control(&text, &control).is_err(),
                "limit {limit}/{peak}"
            );
            assert_eq!(memory.used(), 0);
            assert!(memory.peak() <= limit);
        }
    }
}

#[test]
fn stored_call_input_preserves_both_cancellation_owners_and_admits_before_name_copy() {
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
        assert_eq!(
            FunctionBinding::from_stored_json_with_control(COMPLETE, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
        assert_eq!(memory.peak(), 0);
    }
    let text = format!(
        r#"{{"name":"{}","argument_types":[]}}"#,
        "x".repeat(1 << 20)
    );
    let memory = MemoryBudget::new(128);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let allocations = allocation_counter::measure(|| {
        assert!(FunctionBinding::from_stored_json_with_control(&text, &control).is_err());
    });
    assert!(allocations.bytes_total < 8192, "{allocations:?}");
    assert_eq!(memory.used(), 0);
}
