//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::FunctionOrderSyntax,
    plan::{ExpressionPlan, OwnedScalarExpr},
    ScalarOrder,
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

type Input = ImageDecoder<OwnedScalarExpr, SQLError>;

fn source(kind: usize) -> (ScalarExpr, Vec<Option<ColumnType>>, bool) {
    let (sql, types, explicit) = match kind {
        0 => ("SELECT json_extract_path('{}', 'a', 'b')", vec![ColumnType::Json, ColumnType::Text, ColumnType::Text], false),
        1 => ("SELECT jsonb_extract_path_text('{}', 'a')", vec![ColumnType::JsonB, ColumnType::Text], false),
        2 => ("SELECT json_extract_path('{}', VARIADIC ARRAY['a', 'b'])", vec![ColumnType::Json, ColumnType::Array(Box::new(ColumnType::Text))], true),
        _ => ("SELECT json_extract_path(from_json => '{}', VARIADIC path_elems => ARRAY['a']::varchar[])", vec![ColumnType::Json, ColumnType::Array(Box::new(ColumnType::Varchar(None)))], true),
    };
    let crate::Statement::Select(query) = crate::compile(sql).unwrap().remove(0) else {
        panic!()
    };
    let mut expression = ExpressionPlan::lower(query.projections[0].expr.clone()).scalar;
    let ScalarExpr::Func {
        name,
        args,
        binding,
        filter,
        order_by,
        distinct,
        order_syntax,
    } = &mut expression
    else {
        panic!()
    };
    let decoded = crate::scalar_call_arguments(args).unwrap();
    let names: Vec<_> = decoded
        .iter()
        .map(|argument| argument.name.map(str::to_owned))
        .collect();
    let types: Vec<_> = types.into_iter().map(Some).collect();
    let selected = resolve_call(
        name,
        None,
        RoutineCallDescriptor {
            argument_names: &names,
            argument_types: &types,
            explicit_variadic: explicit,
        },
        None,
        &ProductionControl::uncontrolled(),
    )
    .unwrap()
    .unwrap();
    let mut selected = selected.binding.clone();
    selected.object_id = Some([7; 16]);
    selected.input_collation = crate::catalog::collations::CollationSelection::from_oid(950);
    *binding = Some(selected);
    *filter = Some(Box::new(ScalarExpr::Literal(Value::Str(
        "filter owner".into(),
    ))));
    order_by.push(ScalarOrder {
        expr: ScalarExpr::Literal(Value::Str("order owner".into())),
        descending: true,
        nulls: Some(crate::ast::NullsOrder::Last),
    });
    *distinct = true;
    *order_syntax = FunctionOrderSyntax::Legacy;
    (expression, types, explicit)
}

fn reference(
    mut expression: ScalarExpr,
    types: &[Option<ColumnType>],
    explicit: bool,
) -> ScalarExpr {
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = &mut expression
    else {
        panic!()
    };
    normalize(
        binding,
        args,
        types,
        explicit,
        &mut None,
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    expression
}

fn input(
    source: OwnedScalarExpr,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        super::super::owned::normalize_retained_owned_from(source, &control, &mut input).await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<OwnedScalarExpr>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> Result<(OwnedScalarExpr, usize), SQLError> {
    for polls in 1..400_000 {
        if let Some(value) = advance(input)? {
            return Ok((value, polls));
        }
    }
    panic!("JSON path normalization stopped progressing")
}

fn metadata_addresses(expression: &ScalarExpr) -> [usize; 2] {
    let ScalarExpr::Func {
        filter: Some(filter),
        order_by,
        ..
    } = expression
    else {
        panic!()
    };
    [filter.as_ref(), &order_by[0].expr].map(|expression| {
        let ScalarExpr::Literal(Value::Str(text)) = expression else {
            panic!()
        };
        text.as_ptr() as usize
    })
}

#[test]
fn retained_json_path_normalization_matches_existing_calls_and_preserves_child_owners() {
    for kind in 0..4 {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let (source, types, explicit) = source(kind);
        let expected = reference(source.clone(), &types, explicit);
        let ordinary = super::super::owned::normalize_retained_owned(
            source.copy_for_rewrite(&control).unwrap(),
            &control,
        )
        .unwrap();
        let owned = source.copy_for_rewrite(&control).unwrap();
        let addresses = metadata_addresses(&owned);
        let mut reader = input(owned, &native, &work, &token, &token);
        let (output, polls) = complete(&mut reader).unwrap();
        assert!(polls > 20);
        assert_eq!(*output, expected);
        assert_eq!(*ordinary, expected);
        assert_eq!(output.reserved_bytes(), ordinary.reserved_bytes());
        assert_eq!(metadata_addresses(&output), addresses);
        assert_eq!(
            native.used(),
            output.reserved_bytes() + ordinary.reserved_bytes()
        );
        drop((reader, output, ordinary));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_json_path_normalization_retires_every_interrupted_or_denied_prefix() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (source, _, _) = source(3);
    let (value, polls) = complete(&mut input(
        source.copy_for_rewrite(&control).unwrap(),
        &native,
        &work,
        &token,
        &token,
    ))
    .unwrap();
    drop(value);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls {
        for action in 0..4 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&native, &original, &invoking);
            let mut reader = input(
                source.copy_for_rewrite(&control).unwrap(),
                &native,
                &work,
                &original,
                &invoking,
            );
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            let held =
                (action == 3).then(|| native.reserve(native.limit() - native.used()).unwrap());
            if action == 1 || action == 2 {
                if action == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
            } else if action == 3 {
                match complete(&mut reader) {
                    Ok((value, _)) => drop(value),
                    Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
                }
            }
            let retired = allocation_counter::measure(|| drop(reader));
            assert_eq!(retired.count_total, 0);
            assert_eq!(
                native.used(),
                37 + held.as_ref().map_or(0, MemoryReservation::bytes)
            );
            assert_eq!(work.used(), 0);
            drop(held);
        }
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_json_path_normalization_keeps_invalid_invocations_and_parse_errors_ordered() {
    for defect in 0..6 {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let (mut source, _, _) = source(0);
        let ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = &mut source
        else {
            panic!()
        };
        let invocation = binding.invocation.as_mut().unwrap();
        match defect {
            0 => {
                invocation.argument_sources.pop();
            }
            1 => {
                invocation.argument_targets.pop();
            }
            2 => {
                invocation.argument_positions.pop();
            }
            3 => invocation.argument_positions[1] = 0,
            4 => invocation.argument_positions[1] = 2,
            _ => invocation.argument_targets[0] = "missing_type".into(),
        }
        let error = complete(&mut input(
            source.copy_for_rewrite(&control).unwrap(),
            &native,
            &work,
            &token,
            &token,
        ))
        .unwrap_err();
        if defect == 5 {
            assert_eq!(
                error.to_string(),
                "SQL type `missing_type` is not supported"
            );
        } else {
            assert_eq!(
                error.to_string(),
                "internal error: invalid selected JSON path invocation"
            );
        }
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_json_path_normalization_bounds_wide_calls_and_deep_operand_retirement() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(64 << 20);
            let work = MemoryBudget::new(2 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let (mut source, _, _) = source(0);
            let ScalarExpr::Func {
                args,
                binding: Some(binding),
                ..
            } = &mut source
            else {
                panic!()
            };
            args.truncate(1);
            args.extend((0..4096).map(|index| ScalarExpr::Literal(Value::Str(index.to_string()))));
            let invocation = binding.invocation.as_mut().unwrap();
            invocation.argument_positions = std::iter::once(0)
                .chain(std::iter::repeat_n(1, 4096))
                .collect();
            invocation.argument_targets = std::iter::once("json".into())
                .chain(std::iter::repeat_n("text".into(), 4096))
                .collect();
            invocation.argument_sources = invocation
                .argument_targets
                .iter()
                .cloned()
                .map(Some)
                .collect();
            let mut value = ScalarExpr::Literal(Value::Str("deep path".into()));
            for _ in 0..8192 {
                value = ScalarExpr::Cast {
                    expr: Box::new(value),
                    ty: "text".into(),
                    implicit: true,
                };
            }
            args[1] = value;
            let owned = source.copy_for_rewrite(&control).unwrap();
            drop(crate::plan::retirement::Pending::new(source));
            let mut reader = input(owned, &native, &work, &token, &token);
            let mut maximum_retired = 0;
            let mut result = None;
            for _ in 0..400_000 {
                let mut step = None;
                let allocations = allocation_counter::measure(|| step = Some(advance(&mut reader)));
                maximum_retired = maximum_retired.max(
                    i128::from(allocations.count_total) - i128::from(allocations.count_current),
                );
                if let Some(value) = step.unwrap().unwrap() {
                    result = Some(value);
                    break;
                }
            }
            let output = result.expect("wide normalization completed");
            assert!(
                maximum_retired < 64,
                "one poll retired {maximum_retired} allocations"
            );
            let ScalarExpr::Func { args, .. } = &*output else {
                panic!()
            };
            let ScalarExpr::Array(paths) = &args[1] else {
                panic!()
            };
            assert_eq!(paths.len(), 4096);
            let mut value = &paths[0];
            let mut depth = 0;
            while let ScalarExpr::Cast { expr, .. } = value {
                depth += 1;
                value = expr;
            }
            assert_eq!(depth, 8192);
            let retired = allocation_counter::measure(|| drop((reader, output)));
            assert_eq!(retired.count_total, 0);
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}
