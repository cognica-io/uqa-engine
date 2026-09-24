//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{array_transform, containment, range};
use super::*;
use crate::ast::{FunctionDispatch, RangeFunctionOperation, RangeSubtype};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl, ProductionVec},
    CancellationToken, Value,
};

fn call(name: &str, values: &[Value], control: &ProductionControl<'_>) -> Produced<BindingCall> {
    let name = control.copy_text(name).unwrap();
    let mut args = ProductionVec::new(*control);
    for value in values {
        let (value, memory) = control.copy_value(value).unwrap().into_parts();
        args.push_produced(control.finish(ScalarExpr::Literal(value), memory).unwrap())
            .unwrap();
    }
    let args = args.finish().unwrap();
    let (name, text_memory) = name.into_parts();
    let (arguments, args_memory) = args.into_parts();
    control
        .finish(
            BindingCall {
                name,
                binding: None,
                arguments,
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            },
            control.combine(text_memory, args_memory),
        )
        .unwrap()
}

#[test]
fn auxiliary_bindings_retain_dispatch_names_cast_nodes_and_incoming_arguments() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let source = call("lower", &[Value::Int(1)], &control);
    let mut infer = |_: &ScalarExpr| {
        Ok(Some(
            ColumnType::Range(RangeSubtype::Integer).clone_with_control(&control)?,
        ))
    };
    let output = range::bind_call_with_control(source, &mut infer, &control).unwrap();
    assert_eq!(
        output.binding.as_ref().unwrap().dispatch,
        Some(FunctionDispatch::Range {
            operation: RangeFunctionOperation::Lower,
            subtype: RangeSubtype::Integer,
            multirange: false
        })
    );
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);

    let source = call(
        "contains_op",
        &[Value::Str("{1}".into()), Value::Int(1)],
        &control,
    );
    let ty = ColumnType::Array(Box::new(ColumnType::Integer));
    let mut infer = |_: &ScalarExpr| Ok(Some(ty.clone_with_control(&control)?));
    let output =
        containment::bind_unknown_arguments_with_control(source, &mut infer, &control).unwrap();
    assert!(
        matches!(&output.arguments[0], ScalarExpr::Cast {ty, expr} if ty == "integer[]" && matches!(expr.as_ref(), ScalarExpr::Literal(Value::Str(value)) if value == "{1}"))
    );
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);

    let source = call(
        "array_sort",
        &[Value::Int(1), Value::Str("true".into())],
        &control,
    );
    let ty = ColumnType::Array(Box::new(ColumnType::Json));
    let mut infer = |_: &ScalarExpr| Ok(Some(ty.clone_with_control(&control)?));
    let output = array_transform::bind_call_with_control(source, &mut infer, &control).unwrap();
    assert_eq!(
        output.binding.as_ref().unwrap().dispatch,
        Some(FunctionDispatch::ArraySortJson)
    );
    assert!(matches!(&output.arguments[1], ScalarExpr::Cast {ty, ..} if ty == "boolean"));
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn array_argument_positions_and_borrowed_type_resolution_preserve_named_signatures() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let positions = crate::expr::array_transform_argument_positions_with_control(
        "PG_CATALOG.ARRAY_SORT",
        &[Some("descending"), Some("array")],
        &control,
    )
    .unwrap()
    .unwrap();
    assert_eq!(&*positions, &[1, 0]);
    assert_eq!(budget.used(), positions.reserved_bytes());
    drop(positions);
    assert_eq!(budget.used(), 0);
    let args = [ScalarExpr::Literal(Value::Int(1))];
    let types = [Some(ColumnType::Array(Box::new(ColumnType::Json)))];
    let output = array_transform::resolve_type_with_control(
        "array_reverse",
        None,
        &args,
        &types,
        false,
        &control,
    )
    .unwrap()
    .unwrap();
    assert_eq!(*output, ColumnType::Array(Box::new(ColumnType::Json)));
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
    let args = [
        ScalarExpr::Literal(Value::Int(1)),
        ScalarExpr::Literal(Value::Null),
    ];
    let types = [Some(ColumnType::JsonB), None];
    let output =
        containment::resolve_operator_type_with_control("contains_op", &args, &types, &control)
            .unwrap()
            .unwrap();
    assert_eq!(*output, ColumnType::Boolean);
    assert_eq!(budget.used(), 0);
}

#[test]
fn auxiliary_binders_propagate_inference_resource_failures_and_release_consumed_input() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for sqlstate in ["53200", "57014"] {
        let mut infer = |_: &ScalarExpr| {
            Err(SQLError::Routine {
                sqlstate: sqlstate.into(),
                message: "inference resource failure".into(),
            })
        };
        let source = call("lower", &[Value::Int(1)], &control);
        let error = range::bind_call_with_control(source, &mut infer, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some(sqlstate));
        assert_eq!(budget.used(), 0);
        let source = call("array_sort", &[Value::Int(1)], &control);
        let error =
            array_transform::bind_call_with_control(source, &mut infer, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some(sqlstate));
        assert_eq!(budget.used(), 0);
        let source = call("contains_op", &[Value::Null, Value::Int(1)], &control);
        let error = containment::bind_unknown_arguments_with_control(source, &mut infer, &control)
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some(sqlstate));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn auxiliary_binding_constructors_respect_quota_and_both_cancellation_scopes() {
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let source = call("lower", &[Value::Int(1)], &control);
    let held = budget.reserve(budget.limit() - budget.used()).unwrap();
    let mut infer = |_: &ScalarExpr| {
        Ok(Some(
            ColumnType::Range(RangeSubtype::Integer).clone_with_control(&control)?,
        ))
    };
    let error = range::bind_call_with_control(source, &mut infer, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), held.bytes());
    drop(held);
    assert_eq!(budget.used(), 0);
    for original_cancelled in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let source = call("array_sort", &[Value::Int(1)], &control);
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let mut infer = |_: &ScalarExpr| panic!("cancelled call must not infer arguments");
        let error =
            array_transform::bind_call_with_control(source, &mut infer, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

fn named(name: &str, value: &Value, control: &ProductionControl<'_>) -> Produced<ScalarExpr> {
    let binding =
        FunctionBinding::dispatched_with_control(FunctionDispatch::NamedArgument, control).unwrap();
    let mut args = ProductionVec::new(*control);
    let (name, memory) = control.copy_text(name).unwrap().into_parts();
    args.push_produced(
        control
            .finish(ScalarExpr::Literal(Value::Str(name)), memory)
            .unwrap(),
    )
    .unwrap();
    let (value, memory) = control.copy_value(value).unwrap().into_parts();
    args.push_produced(control.finish(ScalarExpr::Literal(value), memory).unwrap())
        .unwrap();
    let args = args.finish().unwrap();
    let (args, memory) = args.into_parts();
    let (binding, extra) = binding.into_parts();
    control
        .finish(
            ScalarExpr::Func {
                name: String::new(),
                binding: Some(binding),
                args,
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            },
            control.combine(memory, extra),
        )
        .unwrap()
}

#[test]
fn named_array_binding_reorders_owned_expressions_and_casts_destination_slots() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut args = ProductionVec::new(control);
    for (name, value) in [
        ("nulls_first", Value::Null),
        ("array", Value::Int(9)),
        ("descending", Value::Str("false".into())),
    ] {
        args.push_produced(named(name, &value, &control)).unwrap();
    }
    let source = call("array_sort", &[], &control);
    let args = args.finish().unwrap();
    let (args, extra) = args.into_parts();
    let (mut source, memory) = source.into_parts();
    source.arguments = args;
    let source = control
        .finish(source, control.combine(memory, extra))
        .unwrap();
    let ty = ColumnType::Array(Box::new(ColumnType::Json));
    let mut infer = |_: &ScalarExpr| Ok(Some(ty.clone_with_control(&control)?));
    let output = array_transform::bind_call_with_control(source, &mut infer, &control).unwrap();
    assert_eq!(output.arguments[0], ScalarExpr::Literal(Value::Int(9)));
    assert!(
        matches!(&output.arguments[1], ScalarExpr::Cast {ty, expr} if ty == "boolean" && matches!(expr.as_ref(), ScalarExpr::Literal(Value::Str(value)) if value == "false"))
    );
    assert!(
        matches!(&output.arguments[2], ScalarExpr::Cast {ty, expr} if ty == "boolean" && matches!(expr.as_ref(), ScalarExpr::Literal(Value::Null)))
    );
    assert_eq!(
        output.binding.as_ref().unwrap().dispatch,
        Some(FunctionDispatch::ArraySortJson)
    );
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
}

struct FailingResolver(&'static str);

impl super::super::FunctionTypeResolver for FailingResolver {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Err(SQLError::Routine {
            sqlstate: self.0.into(),
            message: "fixture inference failure".into(),
        })
    }
}

#[test]
fn ordinary_optional_array_binding_does_not_panic_on_resolver_resource_errors() {
    for sqlstate in ["53200", "57014"] {
        let resolver = FailingResolver(sqlstate);
        let mut args = vec![ScalarExpr::Func {
            name: "fixture_function".into(),
            binding: None,
            args: Vec::new(),
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        }];
        let original = args.clone();
        let mut binding = None;
        let name = array_transform::bind_call(
            "array_reverse".into(),
            &mut binding,
            &mut args,
            &crate::RowSchema::default(),
            &[],
            Some(&resolver),
        );
        assert_eq!(name, "array_reverse");
        assert!(binding.is_none());
        assert_eq!(args, original);
    }
}

#[test]
fn auxiliary_binders_reject_foreign_input_and_inference_owners_before_noop_returns() {
    let budget = MemoryBudget::new(1 << 20);
    let foreign_budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let foreign = ProductionControl::new(&foreign_budget, &token, &token);
    for owner in 0..3 {
        let source = call("unrecognized", &[], &foreign);
        let mut infer = |_: &ScalarExpr| panic!("no-op branch does not infer");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match owner {
            0 => range::bind_call_with_control(source, &mut infer, &control),
            1 => containment::bind_unknown_arguments_with_control(source, &mut infer, &control),
            _ => array_transform::bind_call_with_control(source, &mut infer, &control),
        }));
        assert!(result.is_err());
        assert_eq!(budget.used(), 0);
        assert_eq!(foreign_budget.used(), 0);
    }
    for (owner, name, args) in [
        (0, "lower", vec![Value::Int(1)]),
        (1, "contains_op", vec![Value::Null, Value::Int(1)]),
        (2, "array_sort", vec![Value::Int(1)]),
    ] {
        let source = call(name, &args, &control);
        let mut infer =
            |_: &ScalarExpr| Ok(Some(ColumnType::Integer.clone_with_control(&foreign)?));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match owner {
            0 => range::bind_call_with_control(source, &mut infer, &control),
            1 => containment::bind_unknown_arguments_with_control(source, &mut infer, &control),
            _ => array_transform::bind_call_with_control(source, &mut infer, &control),
        }));
        assert!(result.is_err());
        assert_eq!(budget.used(), 0);
        assert_eq!(foreign_budget.used(), 0);
    }
}

struct RootOwner {
    call: BindingCall,
    sibling: String,
    memory: Option<MemoryReservation>,
}

impl RootOwner {
    fn new(name: &str, values: &[Value], control: &ProductionControl<'_>) -> Self {
        let call = call(name, values, control);
        let sibling = control.copy_text("untouched sibling expression").unwrap();
        let (call, memory) = call.into_parts();
        let (sibling, extra) = sibling.into_parts();
        Self {
            call,
            sibling,
            memory: control.combine(memory, extra),
        }
    }

    fn bytes(&self) -> usize {
        self.memory.as_ref().map_or(0, MemoryReservation::bytes)
    }
}

fn bind_in_place(
    owner: usize,
    root: &mut RootOwner,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    match owner {
        0 => {
            range::bind_call_in_place_with_control(&mut root.call, &mut root.memory, infer, control)
        }
        1 => containment::bind_unknown_arguments_in_place_with_control(
            &mut root.call,
            &mut root.memory,
            infer,
            control,
        ),
        _ => array_transform::bind_call_in_place_with_control(
            &mut root.call,
            &mut root.memory,
            infer,
            control,
        ),
    }
}

#[test]
fn in_place_bindings_keep_shared_sibling_leases_after_construction_and_failure() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (owner, name, values, ty) in [
        (
            0,
            "lower",
            vec![Value::Int(1)],
            ColumnType::Range(RangeSubtype::Integer),
        ),
        (
            1,
            "contains_op",
            vec![Value::Null, Value::Int(1)],
            ColumnType::JsonB,
        ),
        (
            2,
            "array_sort",
            vec![Value::Int(1), Value::Null],
            ColumnType::Array(Box::new(ColumnType::Json)),
        ),
    ] {
        let mut root = RootOwner::new(name, &values, &control);
        let initial = root.bytes();
        let mut fail = |_: &ScalarExpr| {
            Err(SQLError::Routine {
                sqlstate: "53200".into(),
                message: "inference exhausted its allowance".into(),
            })
        };
        assert_eq!(
            bind_in_place(owner, &mut root, &mut fail, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.used(), initial);
        assert_eq!(root.bytes(), initial);
        assert_eq!(root.sibling, "untouched sibling expression");
        let mut infer = |_: &ScalarExpr| Ok(Some(ty.clone_with_control(&control)?));
        bind_in_place(owner, &mut root, &mut infer, &control).unwrap();
        assert!(root.bytes() > initial);
        assert_eq!(budget.used(), root.bytes());
        assert_eq!(root.sibling, "untouched sibling expression");
        drop(root);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn in_place_quota_and_both_cancellation_errors_preserve_the_root_owner() {
    let budget = MemoryBudget::new(1 << 20);
    for cancel in 0..3 {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut root = RootOwner::new("lower", &[Value::Int(1)], &control);
        let initial = root.bytes();
        let held = (cancel == 0).then(|| budget.reserve(budget.limit() - budget.used()).unwrap());
        if cancel == 1 {
            original.cancel();
        }
        if cancel == 2 {
            invoking.cancel();
        }
        let mut infer = |_: &ScalarExpr| {
            Ok(Some(
                ColumnType::Range(RangeSubtype::Integer).clone_with_control(&control)?,
            ))
        };
        let error = bind_in_place(0, &mut root, &mut infer, &control).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if cancel == 0 { "53200" } else { "57014" })
        );
        assert_eq!(root.bytes(), initial);
        assert_eq!(
            budget.used(),
            initial + held.as_ref().map_or(0, MemoryReservation::bytes)
        );
        assert_eq!(root.sibling, "untouched sibling expression");
        drop(held);
        assert_eq!(budget.used(), initial);
        drop(root);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn in_place_owner_assertions_leave_shared_payloads_charged_during_caught_unwind() {
    let budget = MemoryBudget::new(1 << 20);
    let foreign_budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let foreign = ProductionControl::new(&foreign_budget, &token, &token);
    let mut root = RootOwner::new("lower", &[Value::Int(1)], &control);
    let initial = root.bytes();
    let mut infer = |_: &ScalarExpr| panic!("foreign root must be rejected before inference");
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| bind_in_place(
            0, &mut root, &mut infer, &foreign
        )))
        .is_err()
    );
    assert_eq!(budget.used(), initial);
    let mut infer = |_: &ScalarExpr| {
        Ok(Some(
            ColumnType::Array(Box::new(ColumnType::Integer)).clone_with_control(&foreign)?,
        ))
    };
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| bind_in_place(
            0, &mut root, &mut infer, &control
        )))
        .is_err()
    );
    assert_eq!(budget.used(), initial);
    assert_eq!(foreign_budget.used(), 0);
    assert_eq!(root.sibling, "untouched sibling expression");
    drop(root);
    assert_eq!(budget.used(), 0);
}
