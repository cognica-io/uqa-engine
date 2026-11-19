//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{FunctionCallSyntax, RetainedInputLiteral},
    ir::{RetainedConstantValue, RetainedScalarLiteral},
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

#[test]
fn owned_native_projection_transfers_original_buffers_without_allocation() {
    let name = "original.function".to_owned();
    let name_pointer = name.as_ptr();
    let value = "original evaluated value".repeat(4096);
    let value_pointer = value.as_ptr();
    let expression = Expr::Func {
        name,
        binding: None,
        args: vec![Expr::Literal(Value::Str(value))],
        distinct: true,
        order_by: vec![],
        order_syntax: FunctionCallSyntax::Ordinary,
        filter: Some(Box::new(Expr::Literal(Value::Int(17)))),
    };
    let mut projected = None;
    let allocations = allocation_counter::measure(|| projected = Some(owned(expression)));
    assert_eq!(allocations.count_total, 0);
    let Node::Func {
        name: Source::Owned(name),
        args: Items::Owned(mut args),
        distinct,
        filter: Some(Source::Owned(filter)),
        ..
    } = projected.unwrap()
    else {
        panic!("owned call");
    };
    assert_eq!(name.as_ptr(), name_pointer);
    let argument = args.next().unwrap();
    let Expr::Literal(Value::Str(value)) = &argument else {
        panic!("original argument");
    };
    assert_eq!(value.as_ptr(), value_pointer);
    assert!(distinct);
    assert_eq!(filter, Expr::Literal(Value::Int(17)));
}

#[test]
fn owned_retained_literal_projection_preserves_native_payload_lease() {
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let value = RetainedConstantValue::from_produced(
        control.copy_value(&Value::Str("held".into())).unwrap(),
        &control,
    )
    .unwrap();
    let identity = value.clone();
    let literal =
        RetainedScalarLiteral::with_control(value, None, None, Some(950), &control).unwrap();
    let expression = Expr::RetainedLiteral(Box::new(RetainedInputLiteral(literal)));
    let held = memory.used();
    let mut projected = None;
    let allocations = allocation_counter::measure(|| projected = Some(owned(expression)));
    assert_eq!(allocations.count_total, 0);
    let Node::RetainedLiteral(value) = projected.unwrap() else {
        panic!("native literal");
    };
    assert!(value.value.shares_value_with(&identity));
    assert_eq!(value.collation_oid, Some(950));
    assert_eq!(memory.used(), held);
    drop(identity);
    drop(value);
    assert_eq!(memory.used(), 0);
}
