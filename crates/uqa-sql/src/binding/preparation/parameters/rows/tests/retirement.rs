//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ir::{RetainedConstantValue, RetainedScalarLiteral},
    plan::retirement::tests::ScalarSource,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Reject {
    native: MemoryBudget,
    minimum: Arc<AtomicUsize>,
}

impl ScalarEditIdentity for Reject {
    type Identity = ();
    const BEFORE_CHILDREN: bool = false;
    fn identity(_: &ScalarExpr) {}
}

impl<W: ScalarInputWork> ScalarEdit<W> for Reject {
    async fn apply(
        &mut self,
        (): (),
        expression: OwnedScalarExpr,
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> Result<OwnedScalarExpr, SQLError> {
        assert_eq!(expression.reserved_bytes(), 0);
        self.minimum.store(self.native.used(), Ordering::Relaxed);
        Err(SQLError::Internal("selected field rejected".into()))
    }
}

fn rejected_fields(
    native: &MemoryBudget,
    control: &ProductionControl<'_>,
) -> (OwnedScalarExpr, RowExpansion, Reject, Arc<AtomicUsize>) {
    let minimum = Arc::new(AtomicUsize::new(0));
    let retired = Arc::new(AtomicUsize::new(0));
    let sentinel = ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
        RetainedConstantValue::new(
            Arc::new(ScalarSource {
                native: native.clone(),
                minimum: minimum.clone(),
                retired: retired.clone(),
            }),
            0,
        ),
        None,
        None,
        None,
    ))
    .copy_for_rewrite(control)
    .unwrap();
    assert_eq!(sentinel.reserved_bytes(), 0);
    let null = ScalarExpr::Literal(Value::Null)
        .copy_for_rewrite(control)
        .unwrap();
    let fields = selected_fields([null, sentinel].into_iter(), control);
    assert!(fields.reserved_bytes() > 0);
    let mut expanded = RowExpansion::new(control);
    expanded.push((0, fields), control).unwrap();
    let row = OwnedScalarExpr::row([scalar("discarded", control)].into_iter(), control).unwrap();
    (
        row,
        expanded,
        Reject {
            native: native.clone(),
            minimum,
        },
        retired,
    )
}

#[test]
fn retained_row_expansion_keeps_field_buffer_admission_until_payload_retirement() {
    for suspended in [false, true] {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let (row, expanded, mut edit, retired) = rejected_fields(&native, &control);
        let error = if suspended {
            let native = native.clone();
            let original = token.clone();
            let mut reader = ImageDecoder::new(&work, &token, |mut reader| async move {
                let mut input = reader.input();
                let control = ProductionControl::new(&native, &original, input.cancellation);
                expand_from(row, expanded, &control, &mut input, &mut edit).await
            })
            .unwrap();
            loop {
                match advance(&mut reader) {
                    Ok(None) => {}
                    Ok(Some(_)) => panic!("rejected field cannot finish ROW expansion"),
                    Err(error) => break error,
                }
            }
        } else {
            crate::catalog::work::inline(expand_from(row, expanded, &control, &mut (), &mut edit))
                .unwrap_err()
        };
        assert!(matches!(error, SQLError::Internal(ref text) if text == "selected field rejected"));
        assert_eq!(retired.load(Ordering::Relaxed), 1);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}
