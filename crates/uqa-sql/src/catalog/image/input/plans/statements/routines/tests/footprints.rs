//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{Expr, RetainedInputLiteral},
    ir::RetainedScalarLiteral,
    plan::RelationalPlan,
    ScalarExpr,
};
use uqa_core::Value;

fn literal(bytes: usize) -> RetainedScalarLiteral {
    let control = ProductionControl::uncontrolled();
    RetainedScalarLiteral::from_produced(
        control.copy_value(&Value::Str("x".repeat(bytes))).unwrap(),
        None,
        None,
        None,
        &control,
    )
    .unwrap()
    .0
}

fn registry(default: usize, body: usize) -> RoutineRegistry {
    let original = routine("CREATE FUNCTION f(a text DEFAULT 'default') RETURNS text LANGUAGE SQL RETURN concat('body')", 1);
    let mut definition = original.def.clone();
    definition.params[0].default = Some(Expr::RetainedLiteral(Box::new(RetainedInputLiteral(
        literal(default),
    ))));
    let RoutineBody::Bound(original_body) = &original.body else {
        panic!("bound body")
    };
    let CompiledFunctionBody::SQL(plans) = &***original_body else {
        panic!("SQL body")
    };
    let mut plans = plans.clone();
    let UnifiedPlan::Query(query) = &mut plans[0] else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("query block")
    };
    let ScalarExpr::Func { args, .. } = &mut block.projections[0].expr else {
        panic!("function")
    };
    args[0] = ScalarExpr::RetainedLiteral(literal(body));
    RoutineRegistry::try_from_iter([(
        "public.f".into(),
        vec![SQLUserFunction::new(
            definition,
            RoutineBody::Bound(CompiledFunctionBody::SQL(plans).shared()),
        )
        .shared()],
    )])
    .unwrap()
}

fn captured_bytes(registry: &RoutineRegistry) -> usize {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(2 << 20);
    let saved = registry
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let bytes = memory.used();
    drop(saved);
    assert_eq!(memory.used(), 0);
    bytes
}

fn verify(registry: &RoutineRegistry, baseline: usize, payload: usize) {
    let token = CancellationToken::new();
    let denied = MemoryBudget::new(baseline + payload / 2);
    let error = registry
        .capture(&ProductionControl::new(&denied, &token, &token))
        .err();
    assert_eq!(error.as_ref().and_then(SQLError::sqlstate), Some("53200"));
    assert_eq!(denied.used(), 0);
    assert_eq!(captured_bytes(registry), baseline + payload);
}

#[test]
fn native_routine_capture_admits_original_and_bound_retained_literals() {
    let baseline = captured_bytes(&registry(0, 0));
    for (default, body) in [(64 << 10, 0), (0, 64 << 10), (64 << 10, 64 << 10)] {
        let mut source = registry(default, body);
        verify(&source, baseline, default + body);
        let previous = &source["public.f"][0];
        let replacement =
            SQLUserFunction::new(previous.def.clone(), previous.body.clone()).shared();
        source.insert("public.f".into(), vec![replacement]).unwrap();
        verify(&source, baseline, default + body);
    }
}

#[test]
fn restored_routine_capture_admits_each_retained_literal_tree_once() {
    let empty = encode(&registry(0, 0));
    let base_memory = MemoryBudget::new(2 << 20);
    let token = CancellationToken::new();
    let empty = RoutineRegistry::read_image(&mut empty.as_slice(), &base_memory, &token).unwrap();
    let baseline = captured_bytes(&empty);
    drop(empty);
    assert_eq!(base_memory.used(), 0);
    for (default, body) in [(64 << 10, 0), (0, 64 << 10), (64 << 10, 64 << 10)] {
        let bytes = encode(&registry(default, body));
        let memory = MemoryBudget::new(2 << 20);
        let token = CancellationToken::new();
        let restored = RoutineRegistry::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
        verify(&restored, baseline, default + body);
        assert_eq!(encode(&restored), bytes);
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn restored_native_parameter_literals_keep_their_independent_payload_across_windows() {
    use crate::routines::dependencies::RoutineDeclaration;
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(4 << 20);
    let input_memory = MemoryBudget::new(4 << 20);
    let mut counts = Vec::new();
    for bytes in [8, 512 << 10] {
        let archived = encode(&registry(bytes, 0));
        let restored =
            RoutineRegistry::read_image(&mut archived.as_slice(), &input_memory, &token).unwrap();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut capture = crate::plan::copy::StoredRoutineDefinition::new(
            restored["public.f"][0].clone(),
            &control,
        )
        .unwrap();
        let mut windows = 0;
        while capture.declaration().params.is_empty() {
            capture.copy_next_field(&control).unwrap();
            windows += 1;
        }
        let Some(Expr::RetainedLiteral(literal)) = &capture.declaration().params[0].default else {
            panic!("native parameter")
        };
        let Some(Value::Str(text)) = literal.0.value.resident_value() else {
            panic!("captured native payload")
        };
        assert_eq!(text.len(), bytes);
        drop((capture, restored));
        assert_eq!((memory.used(), input_memory.used()), (0, 0));
        counts.push(windows);
    }
    assert!(counts[1] > counts[0], "{counts:?}");
}

#[test]
fn stored_query_and_window_defaults_copy_native_fields_before_publication() {
    use crate::routines::dependencies::RoutineDeclaration;
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(32 << 20);
    let mut results = Vec::new();
    for window in [false, true] {
        let mut counts = Vec::new();
        for width in [2, 4096] {
            let key = if window { "1" } else { "(1)" };
            let items = std::iter::repeat_n(key, width)
                .collect::<Vec<_>>()
                .join(",");
            let expression = if window {
                format!("row_number() OVER (ORDER BY {items})")
            } else {
                format!("(SELECT v FROM (VALUES {items}) AS s(v))")
            };
            let definition = routine(&format!("CREATE FUNCTION f(v bigint DEFAULT {expression}) RETURNS bigint LANGUAGE sql AS 'SELECT v'"), 1);
            let source =
                RoutineRegistry::try_from_iter([("public.f".into(), vec![definition])]).unwrap();
            let bytes = encode(&source);
            drop(source);
            let input_memory = MemoryBudget::new(32 << 20);
            let decoded =
                RoutineRegistry::read_image(&mut bytes.as_slice(), &input_memory, &token).unwrap();
            let stored = &decoded["public.f"][0];
            if width == 4096 {
                interrupted_default(stored);
            }
            let control = ProductionControl::new(&memory, &token, &token);
            let mut capture =
                crate::routines::StoredRoutineDefinition::new(stored.clone(), &control).unwrap();
            let mut calls = 0;
            while capture.declaration().params.is_empty() {
                capture.copy_next_field(&control).unwrap();
                calls += 1;
            }
            assert_eq!(
                capture.declaration().params[0].default,
                stored.def.params[0].default
            );
            while !capture.copy_next_field(&control).unwrap() {}
            let (actual, actual_children) = capture.into_definition(&control).unwrap();
            let (ordinary, children) =
                crate::plan::copy::capture_stored_with_footprint(&stored.def, &control).unwrap();
            assert_eq!(
                (actual.reserved_bytes(), actual_children),
                (ordinary.reserved_bytes(), children)
            );
            drop((actual, ordinary, decoded));
            assert_eq!((memory.used(), input_memory.used()), (0, 0));
            counts.push(calls);
        }
        eprintln!("stored default window={window}: {counts:?}");
        results.push(counts);
    }
    assert!(
        results.iter().all(|calls| calls[1] >= calls[0] + 128),
        "{results:?}"
    );
}

fn interrupted_default(stored: &SQLRoutine) {
    use crate::routines::dependencies::RoutineDeclaration;
    for cancel in [None, Some(false), Some(true)] {
        let memory = MemoryBudget::new(if cancel.is_some() { 32 << 20 } else { 64 << 10 });
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut capture =
            crate::routines::StoredRoutineDefinition::new(stored.clone(), &control).unwrap();
        if let Some(caller) = cancel {
            assert!(!capture.copy_next_field(&control).unwrap());
            assert!(capture.declaration().params.is_empty());
            if caller {
                invoking.cancel();
            } else {
                original.cancel();
            }
        }
        let error = loop {
            match capture.copy_next_field(&control) {
                Ok(false) => assert!(capture.declaration().params.is_empty()),
                Ok(true) => panic!("the interrupted default was published"),
                Err(error) => break error,
            }
        };
        assert_eq!(
            error.sqlstate(),
            Some(if cancel.is_some() { "57014" } else { "53200" })
        );
        assert!(capture.declaration().params.is_empty());
        drop(capture);
        assert_eq!(memory.used(), 0);
    }
}
