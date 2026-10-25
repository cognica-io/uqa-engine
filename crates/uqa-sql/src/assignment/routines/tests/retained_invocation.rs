//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{FunctionBinding, RoutineInvocationBinding, RoutineVariadicMode, Statement},
    routines::{
        invocation::descriptions::{
            RoutineCallDescriptions, RoutineConversionEnvironment, RoutineDescriptionScope,
        },
        RoutineBody, SQLUserFunction,
    },
};
use std::sync::Arc;
use uqa_core::memory::ProductionControl;

pub(super) fn bound_call() -> (Arc<SQLUserFunction>, FunctionBinding) {
    let Statement::CreateFunction(mut definition) = crate::compile(
        "CREATE FUNCTION public.accept_pair(a integer, b integer) RETURNS integer LANGUAGE SQL AS 'SELECT a + b'",
    ).unwrap().remove(0) else { panic!("function definition") };
    definition.object_id = Some([7; 16]);
    let function = Arc::new(SQLUserFunction::new(*definition, RoutineBody::Source));
    let binding = FunctionBinding {
        object_id: function.def.object_id,
        name: function.def.name.clone(),
        argument_types: vec!["integer".into(), "integer".into()],
        builtin: false,
        dispatch: None,
        invocation: Some(Box::new(RoutineInvocationBinding {
            argument_positions: vec![0, 1],
            argument_targets: vec!["public.accepted_value".into(); 2],
            argument_sources: vec![Some("pg_catalog.int4".into()); 2],
            parameter_types: vec!["public.accepted_value".into(); 2],
            return_type: Some("pg_catalog.int4".into()),
            variadic_mode: RoutineVariadicMode::None,
        })),
        value_binding: None,
        resolution_error: None,
        input_collation: None,
    };
    (function, binding)
}

#[test]
fn retained_invocation_resolves_each_target_once_and_checks_every_value() {
    let context = Context::new();
    let (function, binding) = bound_call();
    let scope = RoutineDescriptionScope::default();
    let state = RoutineCallDescriptions::default();
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    state
        .remember(
            &binding,
            &function,
            &[Some(ColumnType::Integer), Some(ColumnType::Integer)],
            scope.clone(),
            &control,
        )
        .unwrap();
    let saved = state.get(&binding, &scope).unwrap();
    assert!(Arc::ptr_eq(&saved.function().unwrap(), &function));
    let current = || Some(scope.clone());
    let environment = RoutineConversionEnvironment {
        values: &context,
        scope: &current,
        control,
    };
    for index in 0..512 {
        for argument in 0..2 {
            let value = Value::Int(index * 2 + argument as i64);
            assert_eq!(
                saved
                    .coerce_argument(
                        argument,
                        &value,
                        saved.sources()[argument].as_ref(),
                        environment
                    )
                    .unwrap(),
                value
            );
        }
    }
    assert_eq!(context.resolutions.get(), 2);
    assert_eq!(context.checks.get(), 1024);
    context.accept.set(false);
    assert_eq!(
        saved
            .coerce_argument(0, &Value::Int(2000), None, environment)
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
    assert_eq!(context.checks.get(), 1025);
    assert_eq!(context.resolutions.get(), 2);
    assert!(budget.used() > 0 && budget.used() <= 16 * 1024);
    drop(state);
    assert_eq!(budget.used(), 0);
}

#[test]
fn retained_invocation_rejects_changed_bindings_scopes_and_unqualified_names() {
    let (function, binding) = bound_call();
    let state = RoutineCallDescriptions::default();
    let scope = RoutineDescriptionScope::default();
    let sources = [Some(ColumnType::Integer), Some(ColumnType::Integer)];
    let control = ProductionControl::uncontrolled();
    state
        .remember(&binding, &function, &sources, scope.clone(), &control)
        .unwrap();
    assert!(state
        .get(&binding, &RoutineDescriptionScope::default())
        .is_none());
    let mut changed = binding.clone();
    changed.object_id = Some([9; 16]);
    assert!(state.get(&changed, &scope).is_none());
    changed = binding.clone();
    changed
        .invocation
        .as_mut()
        .unwrap()
        .argument_positions
        .swap(0, 1);
    assert!(state.get(&changed, &scope).is_none());
    let fresh = RoutineCallDescriptions::default();
    changed = binding;
    changed.invocation.as_mut().unwrap().argument_targets[0] = "accepted_value".into();
    fresh
        .remember(&changed, &function, &sources, scope.clone(), &control)
        .unwrap();
    assert!(fresh.get(&changed, &scope).is_none());
}

#[test]
fn optional_invocation_retention_declines_small_allowance_and_preserves_cancellation() {
    let (function, binding) = bound_call();
    let state = RoutineCallDescriptions::default();
    let scope = RoutineDescriptionScope::default();
    let sources = [Some(ColumnType::Integer), Some(ColumnType::Integer)];
    let budget = MemoryBudget::new(1);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    state
        .remember(&binding, &function, &sources, scope.clone(), &control)
        .unwrap();
    assert!(state.get(&binding, &scope).is_none());
    assert_eq!(budget.used(), 0);
    token.cancel();
    assert_eq!(
        state
            .remember(&binding, &function, &sources, scope, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn invocation_descriptions_do_not_retain_retired_function_payloads() {
    let (function, binding) = bound_call();
    let state = RoutineCallDescriptions::default();
    let scope = RoutineDescriptionScope::default();
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    state
        .remember(&binding, &function, &[None, None], scope.clone(), &control)
        .unwrap();
    let saved = state.get(&binding, &scope).unwrap();
    assert_eq!(Arc::strong_count(&function), 1);
    assert!(Arc::ptr_eq(&saved.function().unwrap(), &function));
    let retired = Arc::downgrade(&function);
    drop(function);
    assert!(retired.upgrade().is_none());
    assert!(saved.function().is_none());
    drop(state);
    assert_eq!(budget.used(), 0);
}

#[test]
fn conversion_callbacks_invalidate_later_arguments_and_prevent_stale_publication() {
    use std::{cell::RefCell, rc::Rc};
    let context = Context::new();
    let (function, binding) = bound_call();
    let scope = RoutineDescriptionScope::default();
    let current = Rc::new(RefCell::new(scope.clone()));
    let callback_scope = Rc::clone(&current);
    *context.after_check.borrow_mut() = Some(Box::new(move || {
        *callback_scope.borrow_mut() = RoutineDescriptionScope::default();
    }));
    let state = RoutineCallDescriptions::default();
    let control = ProductionControl::uncontrolled();
    state
        .remember(&binding, &function, &[None, None], scope.clone(), &control)
        .unwrap();
    let saved = state.get(&binding, &scope).unwrap();
    let active = || Some(current.borrow().clone());
    let environment = RoutineConversionEnvironment {
        values: &context,
        scope: &active,
        control,
    };
    assert_eq!(
        saved
            .coerce_argument(0, &Value::Int(1), None, environment)
            .unwrap(),
        Value::Int(1)
    );
    assert!(!saved.is_current(active().as_ref()));
    assert_eq!(
        saved
            .coerce_argument(1, &Value::Int(2), None, environment)
            .unwrap(),
        Value::Int(2)
    );
    *context.after_check.borrow_mut() = None;
    *current.borrow_mut() = scope;
    assert_eq!(
        saved
            .coerce_argument(0, &Value::Int(3), None, environment)
            .unwrap(),
        Value::Int(3)
    );
    assert_eq!(
        context.resolutions.get(),
        3,
        "the first callback's stale descriptor was not published"
    );
    assert_eq!(context.checks.get(), 3);
}
