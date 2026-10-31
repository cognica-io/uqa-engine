//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::{EngineHook, EvalContext};
use std::cell::Cell;

#[derive(Default)]
struct SequenceHooks {
    calls: Cell<usize>,
    name_length: Cell<usize>,
    name_pointer: Cell<*const u8>,
    set_value: Cell<Option<(i64, bool)>>,
}

impl SequenceHooks {
    fn observe(&self, name: &str) {
        self.calls.set(self.calls.get() + 1);
        self.name_length.set(name.len());
        self.name_pointer.set(name.as_ptr());
    }
}

impl EngineHook for SequenceHooks {
    fn nextval(&self, name: &str) -> Result<i64> {
        self.observe(name);
        Ok(11)
    }
    fn currval(&self, name: &str) -> Result<i64> {
        self.observe(name);
        Ok(7)
    }
    fn lastval(&self) -> Result<i64> {
        self.observe("");
        Ok(13)
    }
    fn setval(&self, name: &str, value: i64, called: bool) -> Result<i64> {
        self.observe(name);
        self.set_value.set(Some((value, called)));
        Ok(value)
    }
}

#[test]
fn controlled_sequence_dispatch_preserves_bound_and_unbound_session_calls() {
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (name, values, expected, calls, set) in [
        (
            "nextval",
            vec![Value::Str("sequence".into())],
            Value::Int(11),
            1,
            None,
        ),
        ("currval", vec![Value::Int(123)], Value::Int(7), 1, None),
        ("lastval", vec![], Value::Int(13), 1, None),
        (
            "setval",
            vec![Value::Str("sequence".into()), Value::Int(23)],
            Value::Int(23),
            1,
            Some((23, true)),
        ),
        (
            "setval",
            vec![
                Value::Str("sequence".into()),
                Value::Int(29),
                Value::Bool(false),
            ],
            Value::Int(29),
            1,
            Some((29, false)),
        ),
        ("nextval", vec![Value::Null], Value::Null, 0, None),
        (
            "setval",
            vec![
                Value::Str("sequence".into()),
                Value::Null,
                Value::Bool(false),
            ],
            Value::Null,
            0,
            None,
        ),
    ] {
        let binding = FunctionBinding::builtin_by_name(&format!("pg_catalog.{name}"), None);
        for binding in [None, Some(&binding)] {
            let hooks = SequenceHooks::default();
            let context = EvalContext::new(None, &[]).with_engine(&hooks);
            let result = eval_builtin_function_call_with_control(
                name,
                binding,
                args(&values, &control),
                &control,
                &context,
            )
            .unwrap();
            assert_eq!(*result, expected, "{name}");
            assert_eq!(hooks.calls.get(), calls);
            assert_eq!(hooks.set_value.get(), set);
            drop(result);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn controlled_sequence_validation_preserves_error_order_before_session_effects() {
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (name, values) in [
        ("nextval", vec![]),
        ("lastval", vec![Value::Null]),
        (
            "setval",
            vec![Value::Str("sequence".into()), Value::Int(1), Value::Int(1)],
        ),
    ] {
        let hooks = SequenceHooks::default();
        let context = EvalContext::new(None, &[]).with_engine(&hooks);
        let ordinary = crate::expr::eval_builtin_function_call(
            name,
            values.iter().cloned().map(|value| (None, value)).collect(),
            &context,
        )
        .unwrap_err();
        let controlled = eval_builtin_function_call_with_control(
            name,
            None,
            args(&values, &control),
            &control,
            &context,
        )
        .unwrap_err();
        assert_eq!(controlled.sqlstate(), ordinary.sqlstate());
        assert_eq!(controlled.to_string(), ordinary.to_string());
        assert_eq!(hooks.calls.get(), 0);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn controlled_sequence_names_borrow_retained_text_and_admit_formatting_before_effects() {
    let budget = MemoryBudget::new((4 << 20) + 8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let hooks = SequenceHooks::default();
    let context = EvalContext::new(None, &[]).with_engine(&hooks);
    let arguments = args(&[Value::Str("s".repeat(4 << 20))], &control);
    let Value::Str(name) = &arguments[0].1 else {
        panic!("text")
    };
    let pointer = name.as_ptr();
    let measured = allocation_counter::measure(|| {
        assert_eq!(
            *eval_builtin_function_call_with_control(
                "nextval", None, arguments, &control, &context
            )
            .unwrap(),
            Value::Int(11)
        );
    });
    assert!(measured.bytes_total < 4096, "{measured:?}");
    assert_eq!(hooks.name_pointer.get(), pointer);
    assert_eq!(hooks.name_length.get(), 4 << 20);
    assert_eq!(budget.used(), 0);
    let occupied = budget.reserve(budget.available()).unwrap();
    assert_eq!(
        crate::expr::scalar_dispatch::eval_sequence_function_with_control(
            "currval",
            &[Value::Int(123)],
            &context,
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    assert_eq!(hooks.calls.get(), 1);
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(
        *crate::expr::scalar_dispatch::eval_sequence_function_with_control(
            "currval",
            &[Value::Int(123)],
            &context,
            &control
        )
        .unwrap(),
        Value::Int(7)
    );
    assert_eq!(hooks.calls.get(), 2);
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_sequence_cancellation_stops_before_session_effects() {
    for original_cancelled in [false, true] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let sibling = control.copy_text("retained").unwrap();
        let arguments = args(&[Value::Str("sequence".into())], &control);
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let hooks = SequenceHooks::default();
        let context = EvalContext::new(None, &[]).with_engine(&hooks);
        assert_eq!(
            eval_builtin_function_call_with_control("nextval", None, arguments, &control, &context)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(hooks.calls.get(), 0);
        assert_eq!(budget.used(), sibling.reserved_bytes());
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}
