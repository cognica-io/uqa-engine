//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{AnalysisType, ConvertedInput};
use super::*;
use crate::ast::BinaryOp;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, Produced, ProductionVec},
    CancellationToken, Value,
};

mod retirement;

type Fixture = (OwnedScalarExpr, RowExpansion, InputConstants);
type Input = ImageDecoder<OwnedScalarExpr, SQLError>;

fn scalar(text: &str, control: &ProductionControl<'_>) -> OwnedScalarExpr {
    OwnedScalarExpr::literal(
        control.copy_value(&Value::Str(text.into())).unwrap(),
        control,
    )
    .unwrap()
}

fn selected_fields(
    values: impl ExactSizeIterator<Item = OwnedScalarExpr>,
    control: &ProductionControl<'_>,
) -> Produced<Vec<OwnedScalarExpr>> {
    let mut fields = ProductionVec::new(*control);
    fields.reserve(values.len()).unwrap();
    for value in values {
        fields
            .push_produced(control.finish(value, control.empty_reservation()).unwrap())
            .unwrap();
    }
    fields.finish().unwrap()
}

fn fixture(control: &ProductionControl<'_>) -> Fixture {
    let row = OwnedScalarExpr::row(
        [
            scalar("unchanged first", control),
            scalar("removed first", control),
            scalar("unchanged last", control),
            scalar("removed last", control),
        ]
        .into_iter(),
        control,
    )
    .unwrap();
    let fields = selected_fields(
        [
            scalar("same", control),
            OwnedScalarExpr::binary(
                BinaryOp::Add,
                scalar("same", control),
                scalar("same", control),
                control,
            )
            .unwrap(),
            OwnedScalarExpr::row([scalar("nested", control)].into_iter(), control).unwrap(),
        ]
        .into_iter(),
        control,
    );
    let mut constants = InputConstants::new(control);
    let ScalarExpr::Binary { lhs, rhs, .. } = &*fields[1] else {
        unreachable!()
    };
    for (original, text) in [
        (&*fields[0], "first"),
        (&**lhs, "second"),
        (&**rhs, "third"),
    ] {
        constants
            .0
            .insert(
                original.into(),
                ConvertedInput {
                    expression: scalar(text, control),
                    session_dependent: false,
                },
            )
            .unwrap();
    }
    constants
        .5
        .insert(
            AnalysisIdentity::from(&*fields[2]),
            AnalysisType::ordinary(crate::ColumnType::Integer),
        )
        .unwrap();
    let mut expansion = RowExpansion::new(control);
    expansion.push((1, fields), control).unwrap();
    expansion
        .push((3, selected_fields([].into_iter(), control)), control)
        .unwrap();
    (row, expansion, constants)
}

struct Selected(InputConstants);
impl ScalarEditIdentity for Selected {
    type Identity = AnalysisIdentity<ScalarExpr>;
    const BEFORE_CHILDREN: bool = false;
    fn identity(expression: &ScalarExpr) -> Self::Identity {
        expression.into()
    }
}
impl<W: ScalarInputWork> ScalarEdit<W> for Selected
where
    W::Error: From<SQLError> + From<CatalogRetentionError> + Into<SQLError>,
{
    async fn apply(
        &mut self,
        identity: Self::Identity,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedScalarExpr, SQLError> {
        self.0
            .apply_selected_inputs_from(identity, expression, control, work)
            .await
            .map_err(Into::into)
    }
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
        let (row, expanded, constants) = fixture;
        let mut edit = Selected(constants);
        let output = expand_from(row, expanded, &control, &mut input, &mut edit).await?;
        assert!(edit.0.is_consumed());
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
    for polls in 1..200_000 {
        let mut next = None;
        let allocation = allocation_counter::measure(|| next = Some(advance(input)));
        assert!(
            i128::from(allocation.count_total) - i128::from(allocation.count_current) < 64,
            "one work window destroyed an entire ROW subtree"
        );
        if let Some(value) = next.unwrap().unwrap() {
            return (value, polls);
        }
    }
    panic!("ROW expansion stopped progressing")
}

fn deep_scalar(text: &str, control: &ProductionControl<'_>) -> OwnedScalarExpr {
    let mut field = scalar(text, control);
    for _ in 0..8192 {
        field = OwnedScalarExpr::cast(field, control.copy_text("text").unwrap(), true, control)
            .unwrap();
    }
    field
}

fn observe(output: &ScalarExpr) {
    let ScalarExpr::Row(items) = output else {
        panic!("expanded ROW")
    };
    assert_eq!(items.len(), 5);
    for (index, text) in [(0, "unchanged first"), (1, "first"), (4, "unchanged last")] {
        assert_eq!(items[index], ScalarExpr::Literal(Value::Str(text.into())));
    }
    let ScalarExpr::Binary { op, lhs, rhs } = &items[2] else {
        panic!("selected binary field")
    };
    assert_eq!(*op, BinaryOp::Add);
    assert_eq!(**lhs, ScalarExpr::Literal(Value::Str("second".into())));
    assert_eq!(**rhs, ScalarExpr::Literal(Value::Str("third".into())));
    let ScalarExpr::Cast { expr, ty, implicit } = &items[3] else {
        panic!("selected field cast")
    };
    assert_eq!(ty, "pg_catalog.\"int4\"");
    assert!(*implicit);
    assert_eq!(
        **expr,
        ScalarExpr::Row(vec![ScalarExpr::Literal(Value::Str("nested".into()))])
    );
}

#[test]
fn retained_row_expansion_consumes_original_field_identities_in_order() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (row, expanded, mut constants) = fixture(&control);
    let identity = AnalysisIdentity::from(&*row);
    constants.9.insert(identity, expanded).unwrap();
    let ordinary = constants.apply_node_owned(identity, row, &control).unwrap();
    assert!(constants.is_consumed());
    drop(constants);
    let fixture = fixture(&control);
    let ScalarExpr::Row(items) = &*fixture.0 else {
        unreachable!()
    };
    let ScalarExpr::Literal(Value::Str(text)) = &items[0] else {
        unreachable!()
    };
    let address = text.as_ptr();
    let (output, polls) = complete(&mut input(fixture, &native, &work, &token, &token));
    observe(&output);
    assert_eq!(*output, *ordinary);
    assert_eq!(output.reserved_bytes(), ordinary.reserved_bytes());
    let ScalarExpr::Row(items) = &*output else {
        unreachable!()
    };
    let ScalarExpr::Literal(Value::Str(text)) = &items[0] else {
        unreachable!()
    };
    assert_eq!(text.as_ptr(), address);
    assert!(polls > 30);
    drop((output, ordinary));
    assert_eq!((native.used(), work.used()), (0, 0));
}

#[test]
fn retained_row_expansion_retires_each_cancelled_or_unadmitted_prefix() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (output, polls) = complete(&mut input(
        fixture(&control),
        &native,
        &work,
        &token,
        &token,
    ));
    drop(output);
    let sibling = native.reserve(37).unwrap();
    let mut denied = 0;
    for prefix in 0..polls {
        for action in 0..4 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&native, &original, &invoking);
            let mut reader = input(fixture(&control), &native, &work, &original, &invoking);
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
                loop {
                    match advance(&mut reader) {
                        Ok(None) => {}
                        Ok(Some(value)) => {
                            observe(&value);
                            break;
                        }
                        Err(error) => {
                            assert_eq!(error.sqlstate(), Some("53200"));
                            denied += 1;
                            break;
                        }
                    }
                }
            }
            let allocation = allocation_counter::measure(|| drop(reader));
            assert_eq!(allocation.count_total, 0);
            assert_eq!(
                native.used(),
                37 + held
                    .as_ref()
                    .map_or(0, uqa_core::memory::MemoryReservation::bytes)
            );
            assert_eq!(work.used(), 0);
            drop(held);
        }
    }
    assert!(denied > polls / 2);
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_row_expansion_bounds_wide_fields_and_deep_field_edits() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for deep in [false, true] {
                let native = MemoryBudget::new(32 << 20);
                let work = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let control = ProductionControl::new(&native, &token, &token);
                let removed = if deep {
                    deep_scalar("removed", &control)
                } else {
                    scalar("removed", &control)
                };
                let row = OwnedScalarExpr::row([removed].into_iter(), &control).unwrap();
                let fields = if deep {
                    selected_fields([deep_scalar("retained", &control)].into_iter(), &control)
                } else {
                    selected_fields((0..4096).map(|_| scalar("retained", &control)), &control)
                };
                let mut expanded = RowExpansion::new(&control);
                expanded.push((0, fields), &control).unwrap();
                let (output, polls) = complete(&mut input(
                    (row, expanded, InputConstants::new(&control)),
                    &native,
                    &work,
                    &token,
                    &token,
                ));
                assert!(polls >= if deep { 8192 } else { 4096 * 3 });
                let ScalarExpr::Row(items) = &*output else {
                    panic!("expanded ROW")
                };
                assert_eq!(items.len(), if deep { 1 } else { 4096 });
                if deep {
                    let mut field = &items[0];
                    let mut depth = 0;
                    while let ScalarExpr::Cast { expr, .. } = field {
                        depth += 1;
                        field = expr;
                    }
                    assert_eq!(depth, 8192);
                    assert_eq!(*field, ScalarExpr::Literal(Value::Str("retained".into())));
                } else {
                    for field in items {
                        assert_eq!(*field, ScalarExpr::Literal(Value::Str("retained".into())));
                    }
                }
                drop(output);
                assert_eq!((native.used(), work.used()), (0, 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
