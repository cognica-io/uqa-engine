//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_schema_input_admits_frames_and_retains_ast_and_value_payloads() {
    for text in [CALL, ROW, TYPED] {
        let memory = MemoryBudget::new(1 << 24);
        let token = CancellationToken::new();
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output = Some(Expr::from_stored_schema_json(text, &memory, &token, &token).unwrap());
        });
        if text == TYPED {
            assert_eq!(
                memory.used(),
                typed_payload_charge(output.as_ref().unwrap())
            );
        } else {
            assert_eq!(
                usize::try_from(allocations.bytes_current).unwrap(),
                memory.used()
            );
            assert!(
                usize::try_from(allocations.bytes_max).unwrap() <= memory.peak(),
                "{allocations:?}, peak {}",
                memory.peak()
            );
        }
        assert_eq!(output.as_ref().unwrap().reserved_bytes(), memory.used());
        let peak = memory.peak();
        token.cancel();
        assert!(serde_json::to_string(&**output.as_ref().unwrap()).is_ok());
        drop(output);
        assert_eq!(memory.used(), 0);
        for limit in (0..peak).step_by(137).chain([peak - 1]) {
            let memory = MemoryBudget::new(limit);
            let token = CancellationToken::new();
            assert!(
                Expr::from_stored_schema_json(text, &memory, &token, &token).is_err(),
                "limit {limit}/{peak}"
            );
            assert_eq!(memory.used(), 0);
            assert!(memory.peak() <= limit);
        }
    }
}

#[test]
fn stored_schema_input_rejects_cancelled_or_unadmitted_payload_before_allocation() {
    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = Expr::from_stored_schema_json(CALL, &memory, &original, &invoking).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.peak(), 0);
    }
    let text = format!(r#"{{"Literal":"{}"}}"#, "x".repeat(2 << 20));
    let memory = MemoryBudget::new(128);
    let token = CancellationToken::new();
    let allocations = allocation_counter::measure(|| {
        let error = Expr::from_stored_schema_json(&text, &memory, &token, &token).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(allocations.bytes_total < 8192, "{allocations:?}");
    assert_eq!(memory.used(), 0);
    assert!(memory.peak() <= memory.limit());
}

fn typed_payload_charge(actual: &Expr) -> usize {
    // Core's public payload charge excludes B-tree node slack and uses each actual buffer capacity. Measure AST allocations without values, then observe the values through Core's independent footprint traversal.
    let Expr::TypedLiteral {
        value,
        composite_source: Some(source),
        ..
    } = actual
    else {
        panic!("typed fixture")
    };
    let scratch = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let payload = value.retained_payload_bytes(&scratch, &token).unwrap()
        + source
            .value
            .retained_payload_bytes(&scratch, &token)
            .unwrap();
    let mut syntax: serde_json::Value = serde_json::from_str(TYPED).unwrap();
    syntax["TypedLiteral"]["value"] = serde_json::Value::Null;
    syntax["TypedLiteral"]["composite_source"]["value"] = serde_json::Value::Null;
    let text = serde_json::to_string(&syntax).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(Expr::from_stored_schema_json(&text, &memory, &token, &token).unwrap());
    });
    assert_eq!(
        usize::try_from(allocations.bytes_current).unwrap(),
        memory.used()
    );
    let bytes = memory.used();
    drop(output);
    assert_eq!(memory.used(), 0);
    bytes + payload
}

#[test]
fn stored_schema_input_and_retirement_use_a_small_stack_for_deep_expressions() {
    let text = format!(
        r#"{}{{"Literal":true}}{}"#,
        r#"{"Not":"#.repeat(126),
        "}".repeat(126)
    );
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(1 << 24);
            let token = CancellationToken::new();
            let output = Expr::from_stored_schema_json(&text, &memory, &token, &token).unwrap();
            assert!(matches!(&*output, Expr::Not(_)));
            assert_eq!(memory.used(), output.reserved_bytes());
            drop(output);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn stored_schema_input_cancellation_retires_active_input_and_parent_owners() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Barrier,
    };
    let literal = format!(
        r#"{{"Array":[{CALL},{{"Literal":"{}"}}]}}"#,
        "x".repeat(16 << 20)
    );
    let parents = format!(
        r#"{{"Array":[{CALL},{}{{"Literal":0}}]}}"#,
        r#"{"Literal":1},"#.repeat(100_000)
    );
    for text in [literal, parents] {
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
                    while memory.used() < 65536
                        && !done.load(Ordering::Acquire)
                        && std::time::Instant::now() < deadline
                    {
                        std::thread::yield_now();
                    }
                    assert!(memory.used() >= 65536, "no active literal allocation");
                    assert!(!done.load(Ordering::Acquire));
                    if original_cancelled {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                });
                ready.wait();
                let output = Expr::from_stored_schema_json(&text, &memory, &original, &invoking);
                done.store(true, Ordering::Release);
                worker.join().unwrap();
                assert_eq!(output.unwrap_err().sqlstate(), Some("57014"));
            });
            assert_eq!(memory.used(), 0);
        }
    }
}
