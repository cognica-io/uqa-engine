//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn lexical_json_extraction_preserves_source_under_a_small_output_allowance() {
    let text = format!(r#"{{"padding":"{}","a": [10, 20]}}"#, "x".repeat(32_768));
    let budget = MemoryBudget::new(1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    for as_text in [false, true] {
        let output = extract(&text, &[Value::Str("a".into())], as_text, None, &control).unwrap();
        let expected = if as_text {
            Value::Str("[10, 20]".into())
        } else {
            Value::Json("[10, 20]".into())
        };
        assert_eq!(*output, expected);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    let output = extract("[10, 20]", &[Value::Int(-1)], false, Some(true), &control).unwrap();
    assert_eq!(*output, Value::Json("20".into()));
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn lexical_json_errors_and_cancellation_release_every_temporary() {
    let token = CancellationToken::new();
    let keys = [Value::Str("a".into())];
    for (text, state) in [(r#"{"a":[1,]}"#, "22P02"), (r#"{"a":"\u0000"}"#, "22P05")] {
        let budget = MemoryBudget::new(1024);
        let error = extract(
            text,
            &keys,
            false,
            None,
            &ProductionControl::new(&budget, &token, &token),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some(state));
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(0);
    let error = extract(
        r#"{"a":[10, 20]}"#,
        &keys,
        false,
        None,
        &ProductionControl::new(&budget, &token, &token),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = extract(
            r#"{"a":[10, 20]}"#,
            &keys,
            false,
            None,
            &ProductionControl::new(&budget, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}
