//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

fn signature(width: usize, wrong: Option<usize>) -> CatalogReadSnapshot {
    let mut snapshot = empty_catalog().snapshot().clone();
    let mut target = function();
    target.params = vec![target.params.remove(0); width];
    for (position, parameter) in target.params.iter_mut().enumerate() {
        parameter.name = format!("n{position}");
    }
    RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
        .insert(
            "public.renamed".into(),
            vec![SQLUserFunction::new(target, RoutineBody::Source).shared()],
        )
        .unwrap();
    let mut expr = call(1);
    let Expr::Func {
        binding: Some(binding),
        args,
        ..
    } = &mut expr
    else {
        unreachable!()
    };
    args.resize(width, Expr::Literal(uqa_core::Value::Int(1)));
    binding.argument_types = vec!["int4".into(); width];
    if let Some(index) = wrong {
        binding.argument_types[index] = "text".into();
    }
    let mut column = ColumnDef::nullable("value", ColumnType::Integer);
    column.default = Some(expr);
    snapshot.tables.insert(
        RelationIdentity::new("public", "items"),
        table_snapshot([4; 16], vec![column], TableConstraintSet::default()),
    );
    snapshot
}

fn validate_windows(
    snapshot: CatalogReadSnapshot,
    steps: usize,
    stop: Option<(usize, bool)>,
) -> (usize, Result<(), String>) {
    let memory = MemoryBudget::new(8 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let token = original.clone();
    let mut decoder = ImageDecoder::new(&memory, &invoking, move |mut reader| async move {
        let mut input = reader.input();
        validate_with_work(&snapshot, input.memory, &token, &mut input).await
    })
    .unwrap();
    let mut calls = 0;
    let mut held = None;
    let result = loop {
        if let Some((at, deny)) = stop {
            if calls == at {
                if deny {
                    held = Some(memory.reserve(memory.available()).unwrap());
                } else {
                    original.cancel();
                }
            }
        }
        calls += 1;
        match decoder.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps }) {
            Ok(None) => (),
            Ok(Some(())) => break Ok(()),
            Err(error) => break Err(format!("{} {}", error.sqlstate().unwrap(), error)),
        }
    };
    drop(decoder);
    drop(held);
    assert_eq!(memory.used(), 0);
    (calls, result)
}

#[test]
fn original_routine_reference_validation_yields_inside_a_single_selected_signature() {
    let width = 1024;
    let (early, error) = validate_windows(signature(width, Some(0)), 1, None);
    assert!(error
        .as_ref()
        .unwrap_err()
        .contains("bound signature or kind"));
    let (late, late_error) = validate_windows(signature(width, Some(width - 1)), 1, None);
    assert_eq!(late_error, error);
    assert!(late >= early + width - 1, "early {early}, late {late}");
    let (valid, result) = validate_windows(signature(width, None), 1, None);
    result.unwrap();
    assert!(valid > late);
    let (grouped, result) = validate_windows(signature(width, None), 97, None);
    result.unwrap();
    assert!(grouped < valid);
    for (deny, expected) in [(false, "57014"), (true, "53200")] {
        let (_, result) =
            validate_windows(signature(width, None), 1, Some((early + width / 2, deny)));
        assert!(result.unwrap_err().starts_with(expected));
    }
}

#[test]
fn original_routine_body_validation_preserves_source_plan_and_registry_error_order() {
    let target = function();
    let statement = |bad: usize| Statement::Values {
        rows: (0..512)
            .map(|i| vec![Expr::Param(if i == bad { 3 } else { 1 })])
            .collect(),
    };
    let make = |syntax: Statement, plan: Statement| {
        let mut def = target.clone();
        def.body = FunctionBody::Statements(vec![syntax]);
        SQLUserFunction::new(
            def,
            RoutineBody::Bound(CompiledFunctionBody::SQL(vec![UnifiedPlan::lower(plan)]).shared()),
        )
        .shared()
    };
    // The independently retained syntax and plan precede a duplicate identity and the missing caller binding.
    let duplicate = SQLUserFunction::new(target.clone(), RoutineBody::Source).shared();
    let source = |syntax, plan| {
        let mut snapshot = signature(1, Some(0));
        RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
            .insert(
                "public.renamed".into(),
                vec![make(statement(syntax), statement(plan)), duplicate.clone()],
            )
            .unwrap();
        snapshot
    };
    let (first, early) = validate_windows(source(0, 0), 1, None);
    let (later, late) = validate_windows(source(511, 0), 1, None);
    assert!(early.as_ref().unwrap_err().contains("parameter ordinal"));
    assert_eq!(early, late);
    assert!(later >= first + 511);
    let (plan_late, error) = validate_windows(source(512, 511), 1, None);
    assert_eq!(early, error);
    assert!(plan_late > later);
    let (_, error) = validate_windows(source(512, 512), 97, None);
    assert!(error.unwrap_err().contains("duplicate or zero identity"));
}
