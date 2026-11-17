//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FunctionDispatch, ColumnType};

fn nested_fixture(control: &ProductionControl<'_>) -> Fixture {
    let (row, expanded, mut constants) = fixture(control);
    let parameter = ScalarExpr::Column("argument".into())
        .copy_for_rewrite(control)
        .unwrap();
    let call = OwnedScalarExpr::call(
        control.copy_text("json_extract_path").unwrap(),
        None,
        [scalar("{}", control), scalar("leaf", control)].into_iter(),
        control,
    )
    .unwrap();
    let fields = selected_fields([parameter, call].into_iter(), control);
    constants
        .11
        .insert(AnalysisIdentity::from(&*fields[0]), 3)
        .unwrap();
    let selected = crate::type_resolution::json_path::resolve_call(
        "json_extract_path",
        None,
        crate::type_resolution::RoutineCallDescriptor {
            argument_names: &[None, None],
            argument_types: &[Some(ColumnType::Json), Some(ColumnType::Text)],
            explicit_variadic: false,
        },
        None,
        control,
    )
    .unwrap()
    .unwrap();
    let binding = selected.binding.clone_with_control(control).unwrap();
    drop(selected);
    constants
        .1
        .insert(AnalysisIdentity::from(&*fields[1]), binding)
        .unwrap();
    let mut nested = RowExpansion::new(control);
    nested.push((0, fields), control).unwrap();
    let fields = &expanded[0].1;
    constants
        .9
        .insert(AnalysisIdentity::from(&*fields[2]), nested)
        .unwrap();
    (row, expanded, constants)
}

#[test]
fn retained_row_fields_share_complete_parameter_call_and_nested_input_application() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (row, expanded, mut constants) = nested_fixture(&control);
    let identity = AnalysisIdentity::from(&*row);
    constants.9.insert(identity, expanded).unwrap();
    let ordinary = constants.apply_node_owned(identity, row, &control).unwrap();
    assert!(constants.is_consumed());
    drop(constants);
    let (output, polls) = complete(&mut input(
        nested_fixture(&control),
        &native,
        &work,
        &token,
        &token,
    ));
    assert!(polls > 100);
    assert_eq!(*output, *ordinary);
    assert_eq!(output.reserved_bytes(), ordinary.reserved_bytes());
    observe_nested(&output);
    drop((output, ordinary));
    assert_eq!((native.used(), work.used()), (0, 0));
    interruptions(nested_fixture, observe_nested);
}

fn observe_nested(output: &ScalarExpr) {
    let ScalarExpr::Row(items) = output else {
        panic!()
    };
    let ScalarExpr::Cast { expr, .. } = &items[3] else {
        panic!()
    };
    let ScalarExpr::Row(fields) = &**expr else {
        panic!()
    };
    assert_eq!(fields[0], ScalarExpr::Param(3));
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = &fields[1]
    else {
        panic!()
    };
    assert_eq!(
        binding.dispatch,
        Some(FunctionDispatch::JsonPath {
            as_text: false,
            jsonb: false
        })
    );
    assert!(binding.invocation.is_none());
    assert_eq!(args.len(), 2);
    let ScalarExpr::Array(path) = &args[1] else {
        panic!()
    };
    assert_eq!(path.len(), 1);
}

#[test]
fn retained_nested_row_application_uses_independent_constructor_tasks_on_small_stacks() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(128 << 20);
            let work = MemoryBudget::new(2 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let mut row = scalar("leaf", &control);
            let mut expansion = None;
            let mut constants = InputConstants::new(&control);
            for _ in 0..512 {
                let fields = selected_fields([row].into_iter(), &control);
                if let Some(expansion) = expansion.take() {
                    constants
                        .9
                        .insert(AnalysisIdentity::from(&*fields[0]), expansion)
                        .unwrap();
                }
                row = OwnedScalarExpr::row([scalar("discarded", &control)].into_iter(), &control)
                    .unwrap();
                let mut selected = RowExpansion::new(&control);
                selected.push((0, fields), &control).unwrap();
                expansion = Some(selected);
            }
            let (output, polls) = complete(&mut input(
                (row, expansion.unwrap(), constants),
                &native,
                &work,
                &token,
                &token,
            ));
            assert!(polls > 512);
            let mut depth = 0;
            let mut expression = &*output;
            while let ScalarExpr::Row(items) = expression {
                assert_eq!(items.len(), 1);
                expression = &items[0];
                depth += 1;
            }
            assert_eq!(depth, 512);
            assert_eq!(*expression, ScalarExpr::Literal(Value::Str("leaf".into())));
            let retired = allocation_counter::measure(|| drop(output));
            assert_eq!(retired.count_total, 0);
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}
