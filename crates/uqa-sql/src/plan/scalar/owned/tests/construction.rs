//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn typed_null_admits_only_its_type_name_and_freezes_without_copying() {
    for name in ["boolean".to_owned(), "t".repeat(1 << 20)] {
        let budget = MemoryBudget::new(name.len() + 37);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let sibling = budget.reserve(37).unwrap();
        let mut result = None;
        let allocated = allocation_counter::measure(|| {
            result = Some(OwnedScalarExpr::typed_null(&name, &control).unwrap());
        });
        let output = result.unwrap();
        assert_eq!(output.reserved_bytes(), name.len());
        assert_eq!(allocated.bytes_current, i64::try_from(name.len()).unwrap());
        assert_eq!(budget.used(), name.len() + sibling.bytes());
        let ScalarExpr::TypedLiteral {
            value,
            ty,
            bound_type,
            ..
        } = &*output
        else {
            panic!("typed NULL")
        };
        assert_eq!(value, &Value::Null);
        assert_eq!(ty, &name);
        assert!(bound_type.is_none());
        let pointer = ty.as_ptr();
        let output = output.finish(&control).unwrap();
        let ScalarExpr::TypedLiteral { ty, .. } = &*output else {
            unreachable!()
        };
        assert_eq!(ty.as_ptr(), pointer);
        assert_eq!(output.reserved_bytes(), name.len());
        drop((output, sibling));
        assert_eq!(budget.used(), 0);
    }
    let control = ProductionControl::uncontrolled();
    let ordinary = OwnedScalarExpr::typed_null("boolean", &control).unwrap();
    assert_eq!(ordinary.reserved_bytes(), 0);
    assert!(ordinary.finish(&control).unwrap().into_parts().1.is_none());
}

#[test]
fn typed_null_rejects_large_names_and_both_cancellations_before_allocating() {
    let name = "t".repeat(4 << 20);
    for cancelled in 0..3 {
        let budget = MemoryBudget::new(128);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let sibling = budget.reserve(37).unwrap();
        if cancelled == 1 {
            original.cancel();
        }
        if cancelled == 2 {
            invoking.cancel();
        }
        let mut result = None;
        let allocated = allocation_counter::measure(|| {
            result = Some(OwnedScalarExpr::typed_null(&name, &control));
        });
        let error: crate::SQLError = result.unwrap().unwrap_err().into();
        assert_eq!(
            error.sqlstate(),
            Some(if cancelled == 0 { "53200" } else { "57014" })
        );
        if cancelled == 0 {
            assert_eq!(allocated.bytes_total, 0);
        } else {
            // Cancellation may retain its diagnostic text, never the requested type name.
            assert!(allocated.bytes_total < 1024, "{allocated:?}");
        }
        assert_eq!(budget.used(), sibling.bytes());
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn whole_scalar_rewriting_preserves_selected_owners_and_rejects_invalid_results() {
    for mode in 0..7 {
        let budget = MemoryBudget::new(1 << 20);
        let foreign = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let source = ScalarExpr::Row(vec![text(1024), text(4096)]);
        let input = source.copy_for_rewrite(&control).unwrap();
        let sibling = budget.reserve(37).unwrap();
        let foreign_sibling = foreign.reserve(17).unwrap();
        let mut calls = 0;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            input.rewrite(
                &control,
                |input| -> std::result::Result<_, crate::SQLError> {
                    calls += 1;
                    match mode {
                        0 => Err(crate::SQLError::Internal("rewrite failed".into())),
                        1 => panic!("rewrite unwind"),
                        2 => {
                            original.cancel();
                            Ok(input)
                        }
                        3 => {
                            invoking.cancel();
                            Ok(input)
                        }
                        4 => Ok(text(257).copy_for_rewrite(&ProductionControl::new(
                            &foreign, &original, &invoking,
                        ))?),
                        5 => Ok(OwnedScalarExpr::uncontrolled(text(257))),
                        _ => Ok(input.select_child(0)),
                    }
                },
            )
        }));
        assert_eq!(calls, 1);
        match mode {
            1 | 4 | 5 => assert!(result.is_err()),
            0 => assert!(result
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("rewrite failed")),
            2 | 3 => assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014")),
            _ => {
                let output = result.unwrap().unwrap();
                assert_eq!(*output, text(1024));
                assert_eq!(budget.used(), sibling.bytes() + 1024);
                drop(output);
            }
        }
        assert_eq!(budget.used(), sibling.bytes());
        assert_eq!(foreign.used(), foreign_sibling.bytes());
        drop((sibling, foreign_sibling));
        assert_eq!(budget.used(), 0);
        assert_eq!(foreign.used(), 0);
    }
}
