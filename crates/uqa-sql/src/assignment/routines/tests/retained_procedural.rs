//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    plpgsql::{parse_function, PLpgSQLCompilationIdentity, PLpgSQLFunction},
    routines::invocation::descriptions::{
        RoutineCallDescriptions, RoutineConversionEnvironment, RoutineDescriptionScope,
    },
};
use std::{cell::RefCell, rc::Rc};
use uqa_core::memory::ProductionControl;

fn parsed() -> PLpgSQLFunction {
    let crate::Statement::CreateFunction(definition) = crate::compile(
        "CREATE FUNCTION procedural_inputs(a integer, b integer) RETURNS integer LANGUAGE plpgsql AS $$ DECLARE local_value integer; BEGIN RETURN a + b; END $$",
    ).unwrap().remove(0) else { panic!("routine") };
    parse_function(&definition).unwrap()
}

#[test]
fn procedural_metadata_resolves_once_while_values_and_domain_checks_remain_live() {
    let values = Context::new();
    let (function, binding) = super::retained_invocation::bound_call();
    let state = RoutineCallDescriptions::default();
    let scope = RoutineDescriptionScope::default();
    let current = || Some(scope.clone());
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    state
        .remember(&binding, &function, &[None, None], scope.clone(), &control)
        .unwrap();
    let environment = RoutineConversionEnvironment {
        values: &values,
        scope: &current,
        control,
    };
    let parsed = parsed();
    let saved = state.get(&binding, &scope).unwrap();
    let body = saved
        .procedural_body(&parsed, environment)
        .unwrap()
        .unwrap();
    assert_eq!(
        values.resolutions.get(),
        0,
        "entry does not resolve unreached declarations"
    );
    for index in 0..512 {
        assert_eq!(
            body.datum_type(0, "public.accepted_value", environment)
                .unwrap(),
            Some(values.domain.column_type())
        );
        let value = if index % 3 == 0 {
            Value::Null
        } else {
            Value::Int(index)
        };
        assert_eq!(
            body.coerce_datum(0, "public.accepted_value", &value, None, environment)
                .unwrap(),
            value
        );
        assert_eq!(
            body.coerce_result("public.accepted_value", &value, None, environment)
                .unwrap(),
            value
        );
    }
    assert_eq!(
        values.resolutions.get(),
        3,
        "declaration, assignment and return each resolve once"
    );
    assert_eq!(values.checks.get(), 1024);
    values.accept.set(false);
    assert_eq!(
        body.coerce_datum(
            0,
            "public.accepted_value",
            &Value::Int(1),
            None,
            environment
        )
        .unwrap_err()
        .sqlstate(),
        Some("23514")
    );
    assert_eq!(
        body.coerce_result("public.accepted_value", &Value::Int(1), None, environment)
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
    assert_eq!(values.checks.get(), 1026);
    assert_eq!(values.resolutions.get(), 3);
    assert!(budget.used() > 0 && budget.used() <= 16 * 1024);
    drop(state);
    assert_eq!(budget.used(), 0);
}

#[test]
fn procedural_descriptions_bind_compilation_positions_names_and_current_scope() {
    let values = Context::new();
    let (function, binding) = super::retained_invocation::bound_call();
    let state = RoutineCallDescriptions::default();
    let initial = RoutineDescriptionScope::default();
    let current = RefCell::new(initial.clone());
    let scope = || Some(current.borrow().clone());
    let control = ProductionControl::uncontrolled();
    state
        .remember(
            &binding,
            &function,
            &[None, None],
            initial.clone(),
            &control,
        )
        .unwrap();
    let environment = RoutineConversionEnvironment {
        values: &values,
        scope: &scope,
        control,
    };
    let parsed = parsed();
    let saved = state.get(&binding, &initial).unwrap();
    let body = saved
        .procedural_body(&parsed, environment)
        .unwrap()
        .unwrap();
    for index in [0, 1] {
        for _ in 0..256 {
            assert_eq!(
                body.datum_type(index, "pg_catalog.int2", environment)
                    .unwrap(),
                Some(ColumnType::SmallInteger)
            );
        }
    }
    assert_eq!(values.resolutions.get(), 2);
    assert_eq!(
        body.datum_type(0, "pg_catalog.text", environment).unwrap(),
        Some(ColumnType::Text)
    );
    assert_eq!(
        body.datum_type(usize::MAX, "pg_catalog.int4", environment)
            .unwrap(),
        Some(ColumnType::Integer)
    );
    assert_eq!(values.resolutions.get(), 4);
    let mut changed = parsed.clone();
    changed.compilation = PLpgSQLCompilationIdentity::default();
    assert!(saved
        .procedural_body(&changed, environment)
        .unwrap()
        .is_none());
    changed = parsed.clone();
    changed.datums.pop();
    assert!(saved
        .procedural_body(&changed, environment)
        .unwrap()
        .is_none());
    assert!(saved
        .procedural_body(&parsed, environment)
        .unwrap()
        .is_some());
    *current.borrow_mut() = RoutineDescriptionScope::default();
    for _ in 0..2 {
        assert_eq!(
            body.datum_type(0, "pg_catalog.int2", environment).unwrap(),
            Some(ColumnType::SmallInteger)
        );
    }
    assert_eq!(values.resolutions.get(), 6);
    assert!(saved
        .procedural_body(&parsed, environment)
        .unwrap()
        .is_none());
}

#[test]
fn procedural_unresolved_and_unqualified_declarations_keep_live_resolution() {
    let values = Context::new();
    let (function, binding) = super::retained_invocation::bound_call();
    let state = RoutineCallDescriptions::default();
    let scope = RoutineDescriptionScope::default();
    let current = || Some(scope.clone());
    let control = ProductionControl::uncontrolled();
    state
        .remember(&binding, &function, &[None, None], scope.clone(), &control)
        .unwrap();
    let environment = RoutineConversionEnvironment {
        values: &values,
        scope: &current,
        control,
    };
    let parsed = parsed();
    let body = state
        .get(&binding, &scope)
        .unwrap()
        .procedural_body(&parsed, environment)
        .unwrap()
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            body.datum_type(0, "public.missing_type", environment)
                .unwrap(),
            None
        );
        assert_eq!(
            body.datum_type(1, "input_value", environment).unwrap(),
            None
        );
        assert_eq!(
            body.coerce_datum(1, "accepted_value", &Value::Int(7), None, environment)
                .unwrap(),
            Value::Int(7)
        );
    }
    assert_eq!(
        values.resolutions.get(),
        6,
        "neither unresolved results nor namespace-sensitive names are cached"
    );
    assert_eq!(values.checks.get(), 2);
    let mut changed = parsed.clone();
    changed.compilation = PLpgSQLCompilationIdentity::default();
    assert!(state
        .get(&binding, &scope)
        .unwrap()
        .procedural_body(&changed, environment)
        .unwrap()
        .is_none());
}

#[test]
fn procedural_callbacks_invalidate_later_conversions_and_publication() {
    let values = Context::new();
    let (function, binding) = super::retained_invocation::bound_call();
    let state = RoutineCallDescriptions::default();
    let initial = RoutineDescriptionScope::default();
    let current = Rc::new(RefCell::new(initial.clone()));
    let callback = Rc::clone(&current);
    *values.after_check.borrow_mut() = Some(Box::new(move || {
        *callback.borrow_mut() = RoutineDescriptionScope::default();
    }));
    let scope = || Some(current.borrow().clone());
    let control = ProductionControl::uncontrolled();
    state
        .remember(
            &binding,
            &function,
            &[None, None],
            initial.clone(),
            &control,
        )
        .unwrap();
    let environment = RoutineConversionEnvironment {
        values: &values,
        scope: &scope,
        control,
    };
    let parsed = parsed();
    let body = state
        .get(&binding, &initial)
        .unwrap()
        .procedural_body(&parsed, environment)
        .unwrap()
        .unwrap();
    body.coerce_datum(
        0,
        "public.accepted_value",
        &Value::Int(1),
        None,
        environment,
    )
    .unwrap();
    body.coerce_result("public.accepted_value", &Value::Int(2), None, environment)
        .unwrap();
    *current.borrow_mut() = initial.clone();
    body.coerce_datum(
        0,
        "public.accepted_value",
        &Value::Int(3),
        None,
        environment,
    )
    .unwrap();
    assert_eq!(
        values.resolutions.get(),
        3,
        "a callback-invalidated conversion was not published"
    );
    assert_eq!(values.checks.get(), 3);
    values.accept.set(false);
    assert_eq!(
        body.coerce_result("public.accepted_value", &Value::Int(4), None, environment)
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
    assert_eq!(values.resolutions.get(), 4);
    assert_eq!(values.checks.get(), 4);
}

#[test]
fn procedural_retention_declines_exhaustion_and_propagates_cancellation() {
    let values = Context::new();
    let (function, binding) = super::retained_invocation::bound_call();
    let state = RoutineCallDescriptions::default();
    let scope = RoutineDescriptionScope::default();
    let current = || Some(scope.clone());
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    state
        .remember(&binding, &function, &[None, None], scope.clone(), &control)
        .unwrap();
    let environment = RoutineConversionEnvironment {
        values: &values,
        scope: &current,
        control,
    };
    let saved = state.get(&binding, &scope).unwrap();
    let parsed = parsed();
    let full = budget.reserve(budget.limit() - budget.used()).unwrap();
    assert!(saved
        .procedural_body(&parsed, environment)
        .unwrap()
        .is_none());
    drop(full);
    let body = saved
        .procedural_body(&parsed, environment)
        .unwrap()
        .unwrap();
    let used = budget.used();
    let full = budget.reserve(budget.limit() - used).unwrap();
    for _ in 0..2 {
        assert_eq!(
            body.datum_type(0, "public.accepted_value", environment)
                .unwrap(),
            Some(values.domain.column_type())
        );
        assert_eq!(
            body.coerce_datum(
                0,
                "public.accepted_value",
                &Value::Int(3),
                None,
                environment
            )
            .unwrap(),
            Value::Int(3)
        );
        assert_eq!(
            body.coerce_result("public.accepted_value", &Value::Int(4), None, environment)
                .unwrap(),
            Value::Int(4)
        );
    }
    assert_eq!(values.resolutions.get(), 6);
    assert_eq!(values.checks.get(), 4);
    drop(full);
    assert_eq!(
        budget.used(),
        used,
        "failed optional admission releases all partial ownership"
    );
    body.datum_type(0, "public.accepted_value", environment)
        .unwrap();
    token.cancel();
    assert_eq!(
        body.datum_type(0, "public.accepted_value", environment)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(
        body.coerce_result("public.accepted_value", &Value::Null, None, environment)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    drop(state);
    assert_eq!(budget.used(), 0);
}
