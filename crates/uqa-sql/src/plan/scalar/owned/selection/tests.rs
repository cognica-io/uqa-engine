//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FunctionOrderSyntax, ScalarOrder};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn call() -> ScalarExpr {
    ScalarExpr::Func {
        order_syntax: FunctionOrderSyntax::Ordinary,
        name: "selected_call".into(),
        binding: None,
        args: vec![
            ScalarExpr::Literal(Value::Str("x".repeat(1024))),
            ScalarExpr::Column("second".into()),
        ],
        distinct: false,
        order_by: vec![ScalarOrder {
            expr: ScalarExpr::Column("ordered".into()),
            descending: false,
            nulls: None,
        }],
        filter: Some(Box::new(ScalarExpr::Column("filtered".into()))),
    }
}

#[test]
fn call_argument_selection_moves_the_original_vector_and_retires_only_metadata() {
    let source = call();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let ScalarExpr::Func {
        args,
        order_by,
        filter,
        ..
    } = &*input
    else {
        unreachable!();
    };
    let args_pointer = args.as_ptr();
    let order_pointer = order_by.as_ptr();
    let filter_pointer = std::ptr::from_ref(filter.as_deref().unwrap());
    let before = budget.used();
    let mut pair = None;
    let allocated =
        allocation_counter::measure(|| pair = Some(input.take_call_arguments(&control).unwrap()));
    let (parent, arguments) = pair.unwrap();
    assert_eq!(
        allocated.bytes_current,
        i64::try_from(budget.used() - before).unwrap()
    );
    assert_eq!(
        parent.reserved_bytes() + arguments.reserved_bytes(),
        budget.used()
    );
    let ScalarExpr::Or(items) = &*arguments else {
        panic!("selected argument container");
    };
    assert_eq!(items.as_ptr(), args_pointer);
    let mut output = None;
    let allocated = allocation_counter::measure(|| {
        output = Some(parent.replace_call_arguments(arguments, &control).unwrap());
    });
    assert_eq!(allocated.count_total, 0);
    let output = output.unwrap();
    assert_eq!(*output, source);
    assert_eq!(budget.used(), before);
    let ScalarExpr::Func {
        args,
        order_by,
        filter,
        ..
    } = &*output
    else {
        unreachable!();
    };
    assert_eq!(args.as_ptr(), args_pointer);
    assert_eq!(order_by.as_ptr(), order_pointer);
    assert_eq!(
        std::ptr::from_ref(filter.as_deref().unwrap()),
        filter_pointer
    );
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn replacing_arguments_releases_removed_children_and_keeps_ordering_and_filter_owners() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = call().copy_for_rewrite(&control).unwrap();
    let replacement = ScalarExpr::Or(vec![ScalarExpr::Column("replacement".into())])
        .copy_for_rewrite(&control)
        .unwrap();
    let before = budget.used();
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(input.replace_call_arguments(replacement, &control).unwrap());
    });
    assert_eq!(allocation.count_total, 0);
    let output = output.unwrap();
    assert_eq!(
        allocation.bytes_current,
        i64::try_from(budget.used()).unwrap() - i64::try_from(before).unwrap()
    );
    assert_eq!(output.reserved_bytes(), budget.used());
    let ScalarExpr::Func {
        args,
        order_by,
        filter,
        ..
    } = &*output
    else {
        unreachable!();
    };
    assert_eq!(args, &[ScalarExpr::Column("replacement".into())]);
    assert_eq!(order_by[0].expr, ScalarExpr::Column("ordered".into()));
    assert_eq!(
        filter.as_deref(),
        Some(&ScalarExpr::Column("filtered".into()))
    );
    for index in 0..output.child_count() {
        let copy = output.copy_for_rewrite(&control).unwrap();
        let selected = copy.select_child(index);
        assert!(selected.reserved_bytes() > 0);
    }
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn selected_parent_metadata_is_borrowed_while_the_child_keeps_its_owner() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = call().copy_for_rewrite(&control).unwrap();
    let before = budget.used();
    let mut output = None;
    let allocation =
        allocation_counter::measure(|| {
            output = Some(input.map_child_with_parent(0, &control, |parent, child| {
            let ScalarExpr::Func { name, args, .. } = parent else { unreachable!() };
            assert_eq!(name, "selected_call");
            assert_eq!(args[0], ScalarExpr::Literal(Value::Null));
            assert!(matches!(&args[1], ScalarExpr::Column(name) if name.as_str() == "second"));
            Ok::<_, crate::SQLError>(child)
        }).unwrap());
        });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(allocation.bytes_current, 0);
    assert_eq!(budget.used(), before);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn argument_metadata_rejection_and_both_cancellations_destroy_private_inputs() {
    for mode in 0..3 {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = call().copy_for_rewrite(&control).unwrap();
        let sibling = budget.reserve(budget.available()).unwrap();
        if mode == 1 {
            original.cancel();
        }
        if mode == 2 {
            invoking.cancel();
        }
        let error: crate::SQLError = input.take_call_arguments(&control).unwrap_err().into();
        assert_eq!(
            error.sqlstate(),
            Some(if mode == 0 { "53200" } else { "57014" })
        );
        assert_eq!(budget.used(), sibling.bytes());
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}
