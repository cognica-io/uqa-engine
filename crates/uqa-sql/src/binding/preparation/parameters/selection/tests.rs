//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{AnalysisType, ConvertedInput, ExpressionType};
use super::*;
use crate::{
    ast::{FunctionBinding, FunctionDispatch},
    catalog::collations::{derivation::Derivation, CollationSelection},
    ColumnType,
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken, Value,
};

type Input = ImageDecoder<OwnedScalarExpr, SQLError>;
type Fixture = (
    InputConstants,
    AnalysisIdentity<ScalarExpr>,
    OwnedScalarExpr,
);

fn source(kind: usize, control: &ProductionControl<'_>) -> OwnedScalarExpr {
    let text = || {
        OwnedScalarExpr::literal(
            control.copy_value(&Value::Str("original".into())).unwrap(),
            control,
        )
        .unwrap()
    };
    match kind {
        2 | 3 => OwnedScalarExpr::call(
            control.copy_text("collate").unwrap(),
            Some(
                FunctionBinding::dispatched_with_control(
                    FunctionDispatch::Collate { oid: None },
                    control,
                )
                .unwrap(),
            ),
            [
                text(),
                OwnedScalarExpr::literal(
                    control.copy_value(&Value::Str("C".into())).unwrap(),
                    control,
                )
                .unwrap(),
            ]
            .into_iter(),
            control,
        )
        .unwrap(),
        4 => OwnedScalarExpr::typed_literal(
            control.copy_value(&Value::Str("original".into())).unwrap(),
            control.copy_text("text").unwrap(),
            Some(ColumnType::Text.clone_with_control(control).unwrap()),
            None,
            Some(7),
            control,
        )
        .unwrap(),
        8 => OwnedScalarExpr::call(
            control.copy_text("selected").unwrap(),
            Some(FunctionBinding::builtin_by_name_with_control("lower", None, control).unwrap()),
            [text()].into_iter(),
            control,
        )
        .unwrap(),
        _ => text(),
    }
}

fn fixture(kind: usize, control: &ProductionControl<'_>) -> Fixture {
    let expression = source(kind, control);
    let identity = AnalysisIdentity::from(&*expression);
    let mut constants = InputConstants::new(control);
    if matches!(kind, 1 | 7) {
        constants
            .0
            .insert(
                identity,
                ConvertedInput {
                    expression: OwnedScalarExpr::typed_literal(
                        control.copy_value(&Value::Str("selected".into())).unwrap(),
                        control.copy_text("text").unwrap(),
                        Some(ColumnType::Text.clone_with_control(control).unwrap()),
                        None,
                        Some(7),
                        control,
                    )
                    .unwrap(),
                    session_dependent: true,
                },
            )
            .unwrap();
    }
    if matches!(kind, 2 | 3) {
        constants.3.select(identity, None, 950).unwrap();
        if kind == 3 {
            constants
                .3
                .coerce(Some(identity), &ColumnType::Integer)
                .unwrap();
        }
    }
    if matches!(kind, 4 | 5 | 7) {
        let ty = if kind == 5 {
            ColumnType::Domain {
                schema: "schema_型".repeat(900),
                name: "domain_🙂".repeat(900),
                oid: 37,
                array_oid: Some(41),
                collation_oid: Some(100),
                base: Box::new(ColumnType::Text),
            }
        } else {
            ColumnType::Text
        };
        let mut selected = ExpressionType::resolved_owned(Some(
            AnalysisType::from_produced(ty.clone_with_control(control).unwrap(), control).unwrap(),
        ));
        selected.derivation = if kind == 5 {
            Derivation::Explicit(950)
        } else {
            Derivation::Implicit(950)
        };
        selected.initial_collation = 950;
        constants.4.enabled = true;
        constants.4.record(&expression, &selected).unwrap();
    }
    if matches!(kind, 6 | 7) {
        let target = if kind == 6 {
            ColumnType::Array(Box::new(ColumnType::Named("\"型🙂\"".repeat(900))))
        } else {
            ColumnType::Varchar(Some(17))
        };
        constants
            .5
            .insert(
                identity,
                AnalysisType::from_produced(target.clone_with_control(control).unwrap(), control)
                    .unwrap(),
            )
            .unwrap();
    }
    if kind == 8 {
        constants
            .6
            .insert(identity, CollationSelection::from_oid(950))
            .unwrap();
    }
    (constants, identity, expression)
}

fn input(
    fixture: Fixture,
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
        let (mut constants, identity, expression) = fixture;
        let output = constants
            .apply_selected_inputs_from(identity, expression, &control, &mut input)
            .await?;
        assert!(constants.is_consumed());
        Ok(output)
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<OwnedScalarExpr>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> (OwnedScalarExpr, usize) {
    for polls in 1..100_000 {
        if let Some(output) = advance(input).unwrap() {
            return (output, polls);
        }
    }
    panic!("selected inputs stopped progressing")
}

fn observe(kind: usize, expression: &ScalarExpr) {
    match kind {
        0 | 3 => assert_eq!(
            *expression,
            ScalarExpr::Literal(Value::Str("original".into()))
        ),
        1 | 4 => {
            let ScalarExpr::TypedLiteral {
                value,
                collation_oid,
                parameter_index,
                ..
            } = expression
            else {
                panic!("selected literal");
            };
            assert_eq!(
                *value,
                Value::Str(if kind == 1 { "selected" } else { "original" }.into())
            );
            assert_eq!(*collation_oid, if kind == 4 { Some(950) } else { None });
            assert_eq!(*parameter_index, Some(7));
        }
        2 => {
            let clause = crate::catalog::collations::expression::scalar_clause(expression)
                .unwrap()
                .unwrap();
            assert_eq!(clause.oid, Some(950));
            assert_eq!(clause.name, "C");
            assert_eq!(
                *clause.argument,
                ScalarExpr::Literal(Value::Str("original".into()))
            );
        }
        5 => {
            let ScalarExpr::Collated {
                expr,
                ty,
                collation,
            } = expression
            else {
                panic!("selected result");
            };
            assert!(matches!(ty, ColumnType::Domain { oid: 37, .. }));
            assert_eq!(collation.derivation(), Derivation::Explicit(950));
            assert_eq!(**expr, ScalarExpr::Literal(Value::Str("original".into())));
        }
        6 | 7 => {
            let ScalarExpr::Cast { expr, ty, implicit } = expression else {
                panic!("selected cast");
            };
            assert!(*implicit);
            if kind == 6 {
                assert_eq!(ty, &format!("{}[]", "\"型🙂\"".repeat(900)));
            } else {
                assert_eq!(ty, "pg_catalog.\"varchar\"(17)");
                assert!(
                    matches!(&**expr, ScalarExpr::TypedLiteral { value: Value::Str(value), collation_oid: Some(950), parameter_index: Some(7), .. } if value == "selected")
                );
            }
        }
        8 => {
            let ScalarExpr::Func {
                binding: Some(binding),
                ..
            } = expression
            else {
                panic!("selected call");
            };
            assert_eq!(binding.input_collation, CollationSelection::from_oid(950));
        }
        _ => unreachable!(),
    }
}

#[test]
fn selected_input_application_preserves_order_and_every_suspended_owner() {
    for kind in 0..9 {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let (mut constants, identity, expression) = fixture(kind, &control);
        let ordinary = constants
            .apply_node_owned(identity, expression, &control)
            .unwrap();
        assert!(constants.is_consumed());
        drop(constants);
        let (output, polls) = complete(&mut input(
            fixture(kind, &control),
            &native,
            &work,
            &token,
            &token,
        ));
        observe(kind, &output);
        assert_eq!(*ordinary, *output);
        assert_eq!(ordinary.reserved_bytes(), output.reserved_bytes());
        drop((ordinary, output));
        let sibling = native.reserve(37).unwrap();
        for prefix in 0..polls {
            for cancellation in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let control = ProductionControl::new(&native, &original, &invoking);
                let mut reader = input(
                    fixture(kind, &control),
                    &native,
                    &work,
                    &original,
                    &invoking,
                );
                for _ in 0..prefix {
                    assert!(advance(&mut reader).unwrap().is_none());
                }
                if cancellation != 0 {
                    if cancellation == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
                }
                let allocation = allocation_counter::measure(|| drop(reader));
                assert_eq!(allocation.count_total, 0);
                assert_eq!((native.used(), work.used()), (37, 0));
            }
        }
        drop(sibling);
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn selected_input_application_retires_replaced_trees_and_rejects_unadmitted_wrappers() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(32 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let (constants, identity, mut expression) = fixture(1, &control);
            for _ in 0..8192 {
                expression = OwnedScalarExpr::cast(
                    expression,
                    control.copy_text("text").unwrap(),
                    true,
                    &control,
                )
                .unwrap();
            }
            let (output, polls) = complete(&mut input(
                (constants, identity, expression),
                &native,
                &work,
                &token,
                &token,
            ));
            observe(1, &output);
            assert!(polls > 8192);
            drop(output);
            assert_eq!((native.used(), work.used()), (0, 0));
            for kind in [2, 5, 6] {
                let fixture = fixture(kind, &control);
                let held = native.reserve(native.limit() - native.used()).unwrap();
                let mut reader = input(fixture, &native, &work, &token, &token);
                let error = loop {
                    match advance(&mut reader) {
                        Err(error) => break error,
                        Ok(None) => {}
                        Ok(Some(_)) => panic!("unadmitted selected input {kind}"),
                    }
                };
                assert_eq!(error.sqlstate(), Some("53200"));
                drop(reader);
                assert_eq!(native.used(), held.bytes());
                drop(held);
            }
            // Consumed observation slots and the replaced literal release enough admitted storage for this small final wrapper.
            let fixture = fixture(7, &control);
            let held = native.reserve(native.limit() - native.used()).unwrap();
            let (output, _) = complete(&mut input(fixture, &native, &work, &token, &token));
            observe(7, &output);
            drop(output);
            assert_eq!(native.used(), held.bytes());
            drop(held);
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

fn deep_fixture(
    literal: bool,
    shared: bool,
    control: &ProductionControl<'_>,
) -> (Fixture, Option<AnalysisType>) {
    let mut ty = ColumnType::Text;
    for _ in 0..8192 {
        ty = ColumnType::Array(Box::new(ty));
    }
    let expression = OwnedScalarExpr::cast(
        OwnedScalarExpr::literal(control.copy_value(&Value::Null).unwrap(), control).unwrap(),
        control.copy_text("text[]").unwrap(),
        false,
        control,
    )
    .unwrap();
    let identity = AnalysisIdentity::from(&*expression);
    let mut constants = InputConstants::new(control);
    let mut selected = ExpressionType::resolved(None);
    selected.ty = Some(
        AnalysisType::from_produced(ty.clone_with_control(control).unwrap(), control).unwrap(),
    );
    selected.derivation = Derivation::Implicit(950);
    selected.initial_collation = 950;
    constants.4.enabled = true;
    constants.4.record(&expression, &selected).unwrap();
    if literal {
        constants
            .0
            .insert(
                identity,
                ConvertedInput {
                    expression: OwnedScalarExpr::typed_literal(
                        control.copy_value(&Value::Null).unwrap(),
                        control.copy_text("text[]").unwrap(),
                        Some(ty.clone_with_control(control).unwrap()),
                        None,
                        None,
                        control,
                    )
                    .unwrap(),
                    session_dependent: false,
                },
            )
            .unwrap();
    }
    let retained = shared.then(|| selected.ty.take().unwrap());
    drop(selected);
    ((constants, identity, expression), retained)
}

#[test]
fn selected_result_types_share_readers_and_bound_validation_copy_and_retirement() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for literal in [false, true] {
                let mut poll_counts = Vec::new();
                for shared in [false, true] {
                    let native = MemoryBudget::new(32 << 20);
                    let work = MemoryBudget::new(1 << 20);
                    let token = CancellationToken::new();
                    let control = ProductionControl::new(&native, &token, &token);
                    let (fixture, retained) = deep_fixture(literal, shared, &control);
                    let retained_bytes = retained.as_ref().map_or(0, |ty| match ty {
                        AnalysisType::Shared(ty) => ty.reserved_bytes(),
                        AnalysisType::Inline(_) => panic!("nested selected type"),
                    });
                    let mut reader = input(fixture, &native, &work, &token, &token);
                    let mut polls = 0;
                    let output = loop {
                        polls += 1;
                        assert!(polls < 100_000);
                        let mut next = None;
                        let allocation =
                            allocation_counter::measure(|| next = Some(advance(&mut reader)));
                        assert!(
                            i128::from(allocation.count_total)
                                - i128::from(allocation.count_current)
                                < 64,
                            "one poll destroyed a complete type"
                        );
                        if let Some(output) = next.unwrap().unwrap() {
                            break output;
                        }
                    };
                    poll_counts.push(polls);
                    let result_type = match &*output {
                        ScalarExpr::Collated { ty, collation, .. } if !literal => {
                            assert_eq!(collation.implicit_oid(), Some(950));
                            ty
                        }
                        ScalarExpr::TypedLiteral {
                            bound_type: Some(ty),
                            collation_oid: Some(950),
                            ..
                        } if literal => ty,
                        _ => panic!("selected nested result"),
                    };
                    let mut leaf = result_type;
                    let mut dimensions = 0;
                    while let ColumnType::Array(child) = leaf {
                        leaf = child;
                        dimensions += 1;
                    }
                    assert_eq!(dimensions, 8192);
                    assert_eq!(*leaf, ColumnType::Text);
                    drop((output, reader));
                    assert_eq!(native.used(), retained_bytes);
                    drop(retained);
                    assert_eq!((native.used(), work.used()), (0, 0));
                }
                assert!(poll_counts[0] >= poll_counts[1] + 8192);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
