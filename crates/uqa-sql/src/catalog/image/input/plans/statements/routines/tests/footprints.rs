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
