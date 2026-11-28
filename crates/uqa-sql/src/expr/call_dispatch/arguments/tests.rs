//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::{eval_function_call, EngineHook, EvalContext};
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Observer(Vec<usize>);

impl EngineHook for Observer {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }

    fn call_scalar_function(&self, name: &str, args: &[Value]) -> Option<Result<Value>> {
        (name == "observe_positional").then(|| {
            Ok(Value::Bool(
            self.0.len() == args.len() && self.0.iter().zip(args).all(|(pointer, value)| {
                matches!(value, Value::Str(text) if text.as_ptr() as usize == *pointer)
            })
        ))
        })
    }

    fn call_user_function(
        &self,
        name: &str,
        args: &[(Option<String>, Value)],
    ) -> Option<Result<Value>> {
        (name == "observe_named").then(|| Ok(Value::Bool(
            self.0.len() == args.len() && self.0.iter().zip(args).enumerate().all(|(index, (pointer, (name, value)))| {
                name.as_deref() == Some(&format!("p{index}")) && matches!(value, Value::Str(text) if text.as_ptr() as usize == *pointer)
            })
        )))
    }
}

#[test]
fn session_callbacks_receive_original_positional_and_named_payloads_at_every_arity() {
    for arity in 1..=6 {
        for named in [false, true] {
            let text = (0..arity)
                .map(|index| {
                    if index == 0 {
                        "x".repeat(4 * 1024 * 1024)
                    } else {
                        format!("argument {index}")
                    }
                })
                .collect::<Vec<_>>();
            let hook = Observer(text.iter().map(|value| value.as_ptr() as usize).collect());
            let arguments = text
                .into_iter()
                .enumerate()
                .map(|(index, value)| (named.then(|| format!("p{index}")), Value::Str(value)))
                .collect();
            let name = if named {
                "observe_named"
            } else {
                "observe_positional"
            };
            assert_eq!(
                eval_function_call(
                    name,
                    arguments,
                    &EvalContext::new(None, &[]).with_engine(&hook)
                )
                .unwrap(),
                Value::Bool(true)
            );
        }
    }
}

#[test]
fn four_positional_slots_use_no_heap_and_refused_growth_releases_every_owner() {
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let arguments = (0..4).map(|index| (None, Value::Int(index))).collect();
    let mut source = Some(
        control
            .finish(arguments, control.empty_reservation())
            .unwrap(),
    );
    let allocation = allocation_counter::measure(|| {
        let moved = MovedArguments::new(source.take().unwrap(), &control).unwrap();
        assert_eq!(moved.values.len(), 4);
        assert_eq!(moved.values[3], Value::Int(3));
        let restored = moved.into_pairs(&control).unwrap();
        assert_eq!(restored[3], (None, Value::Int(3)));
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    let arguments = (0..5).map(|index| (None, Value::Int(index))).collect();
    let error = MovedArguments::new(
        control
            .finish(arguments, control.empty_reservation())
            .unwrap(),
        &control,
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
}
