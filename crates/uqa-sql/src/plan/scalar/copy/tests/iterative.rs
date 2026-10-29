//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::schema::retention::CatalogRetentionError;
use uqa_core::memory::MemoryError;

fn deep(depth: usize) -> ScalarExpr {
    let mut expression = ScalarExpr::Literal(Value::Str("leaf payload".into()));
    for _ in 0..depth {
        expression = ScalarExpr::And(vec![expression]);
    }
    expression
}

fn verify_depth(mut expression: &ScalarExpr, depth: usize) {
    for _ in 0..depth {
        let ScalarExpr::And(children) = expression else {
            panic!("conjunction")
        };
        assert_eq!(children.len(), 1);
        expression = &children[0];
    }
    assert_eq!(
        expression,
        &ScalarExpr::Literal(Value::Str("leaf payload".into()))
    );
}

#[test]
fn deep_mutable_and_immutable_scalar_copies_use_bounded_thread_stack() {
    for depth in [128, 1024] {
        verify_copies(depth);
    }
}

fn verify_copies(depth: usize) {
    let source = deep(depth);
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut mutable = None;
    let allocated = allocation_counter::measure(|| {
        mutable = Some(source.copy_for_rewrite(&control).unwrap());
    });
    let mutable = mutable.unwrap();
    verify_depth(&mutable, depth);
    assert_eq!(budget.used(), mutable.reserved_bytes());
    assert_eq!(
        allocated.bytes_current,
        i64::try_from(budget.used()).unwrap()
    );
    assert!(allocated.bytes_max > u64::try_from(budget.used()).unwrap());
    drop(mutable);
    assert_eq!(budget.used(), 0);
    let mut immutable = None;
    let allocated = allocation_counter::measure(|| {
        immutable = Some(source.clone_with_control(&control).unwrap());
    });
    let immutable = immutable.unwrap();
    verify_depth(&immutable, depth);
    assert_eq!(budget.used(), immutable.reserved_bytes());
    assert_eq!(
        allocated.bytes_current,
        i64::try_from(budget.used()).unwrap()
    );
    assert!(allocated.bytes_max > u64::try_from(budget.used()).unwrap());
    drop(immutable);
    assert_eq!(budget.used(), 0);
    let ordinary = ProductionControl::uncontrolled();
    let mutable = source.copy_for_rewrite(&ordinary).unwrap();
    verify_depth(&mutable, depth);
    assert_eq!(mutable.reserved_bytes(), 0);
    let immutable = source.clone_with_control(&ordinary).unwrap();
    verify_depth(&immutable, depth);
    assert_eq!(immutable.reserved_bytes(), 0);
}

#[test]
fn deep_scalar_copy_rejects_workspace_before_allocation_and_preserves_independent_owners() {
    let source = deep(1024);
    for mutable in [false, true] {
        let budget = MemoryBudget::new(8192);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let retained = control.copy_text("independent owner").unwrap();
        let before = budget.used();
        let measured = allocation_counter::measure(|| {
            let result = if mutable {
                source.copy_for_rewrite(&control).map(drop)
            } else {
                source.clone_with_control(&control).map(drop)
            };
            assert!(matches!(result, Err(CatalogRetentionError::Memory(_))));
        });
        assert!(measured.bytes_max <= u64::try_from(8192 - before).unwrap());
        assert_eq!(measured.bytes_current, 0);
        assert_eq!(budget.used(), before);
        assert!(budget.peak() <= 8192);
        assert_eq!(&*retained, "independent owner");
        verify_depth(&source, 1024);
        drop(retained);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn late_scalar_metadata_failure_releases_completed_children_without_copying_large_fields() {
    let text = || ScalarExpr::Literal(Value::Str("x".repeat(97)));
    let sources = [
        ScalarExpr::Cast {
            expr: Box::new(text()),
            ty: "t".repeat(4 << 20),
            implicit: true,
        },
        ScalarExpr::CompositeRow {
            items: vec![text()],
            binding: crate::ast::CompositeRowBinding {
                layout: None,
                ty: "t".repeat(4 << 20),
                attributes: vec![1],
                argument_types: Some(vec![ColumnType::Text]),
            },
            bound_type: None,
        },
    ];
    for source in sources {
        let expected = source.clone();
        for mutable in [false, true] {
            let budget = MemoryBudget::new(4096);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let measured = allocation_counter::measure(|| {
                let result = if mutable {
                    source.copy_for_rewrite(&control).map(drop)
                } else {
                    source.clone_with_control(&control).map(drop)
                };
                let Err(CatalogRetentionError::Memory(MemoryError::Limit { required, .. })) =
                    result
                else {
                    panic!("late metadata must exceed the allowance")
                };
                assert!((4 << 20..(4 << 20) + 4096).contains(&required));
            });
            assert!(measured.bytes_total >= 97);
            assert!(measured.bytes_max <= 4096);
            assert_eq!(measured.bytes_current, 0);
            assert_eq!(budget.used(), 0);
            assert_eq!(source, expected);
        }
    }
}
