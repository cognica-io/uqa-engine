//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::ColumnType, plan::ExpressionPlan, SQLError};
use std::sync::Arc;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

type Input = ImageDecoder<OwnedScalarExpr, SQLError>;

fn source(expression: ScalarExpr) -> Arc<OwnedScalarExpr> {
    Arc::new(OwnedScalarExpr {
        expression,
        memory: None,
    })
}

fn input(
    source: Arc<OwnedScalarExpr>,
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
        source.copy_for_rewrite_from(&control, &mut input).await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> std::result::Result<Option<OwnedScalarExpr>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> (OwnedScalarExpr, usize) {
    for polls in 1..1_000_000 {
        if let Some(value) = advance(input).unwrap() {
            return (value, polls);
        }
    }
    panic!("retained scalar copy stopped progressing")
}

fn scalar(sql: &str) -> ScalarExpr {
    let crate::Statement::Select(mut query) = crate::compile(sql).unwrap().remove(0) else {
        panic!("SELECT");
    };
    ExpressionPlan::lower(query.projections.remove(0).expr).scalar
}

#[test]
fn retained_scalar_copy_preserves_analyzed_fields_and_movable_children() {
    let native = MemoryBudget::new(4 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let fixture = source(ScalarExpr::Row(vec![
        scalar("SELECT CASE a WHEN 1 THEN sum(b) FILTER (WHERE c) OVER (PARTITION BY d ORDER BY e ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING) ELSE CAST(f AS text) END"),
        scalar("SELECT concat(a, '한🙂')"),
        scalar("SELECT a IN (1,2,3)"),
        scalar("SELECT ARRAY[1,2,3]"),
        ScalarExpr::TypedLiteral {
            value: Value::Str("payload_é🦀".repeat(1024)),
            ty: "text".into(),
            bound_type: Some(ColumnType::Array(Box::new(ColumnType::Varchar(Some(32))))),
            parameter_index: Some(7),
            collation_oid: Some(950),
            composite_source: None,
        },
        ScalarExpr::CompositeRow {
            items: vec![ScalarExpr::Literal(Value::Int(3))],
            binding: crate::ast::CompositeRowBinding {
                layout: None, ty: "composite#20001".into(), attributes: vec![2],
                argument_types: Some(vec![ColumnType::Integer]),
            },
            bound_type: None,
        },
        ScalarExpr::InSubquery { expr: Box::new(ScalarExpr::Param(3)), subquery: 7, negated: true },
        ScalarExpr::Collated { expr: Box::new(text(29)), ty: ColumnType::Text, collation: crate::ir::ResultCollation::new(crate::catalog::collations::derivation::Derivation::Implicit(950), 950) },
    ]));
    let ordinary = fixture.copy_for_rewrite(&control).unwrap();
    let (value, polls) = complete(&mut input(fixture.clone(), &native, &work, &token, &token));
    assert!(polls > 100);
    assert_eq!(*value, **fixture);
    assert_eq!(*value, *ordinary);
    assert_eq!(value.reserved_bytes(), ordinary.reserved_bytes());
    drop(ordinary);
    let selected = value.select_child(4);
    assert_eq!(
        *selected,
        match &**fixture {
            ScalarExpr::Row(items) => items[4].clone(),
            _ => unreachable!(),
        }
    );
    assert_eq!(native.used(), selected.reserved_bytes());
    drop(selected);
    assert_eq!((native.used(), work.used()), (0, 0));
}

#[test]
fn retained_scalar_copy_retires_every_interrupted_constructor_prefix() {
    let fixture = source(ScalarExpr::Row(vec![
        scalar("SELECT CASE WHEN a THEN sum(b ORDER BY c) FILTER (WHERE d) ELSE 'end' END"),
        ScalarExpr::Cast {
            expr: Box::new(text(37)),
            ty: "typename_한".repeat(1024),
            implicit: true,
        },
    ]));
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (value, polls) = complete(&mut input(fixture.clone(), &native, &work, &token, &token));
    drop(value);
    let sibling = native.reserve(29).unwrap();
    for prefix in 0..polls - 1 {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(fixture.clone(), &native, &work, &original, &invoking);
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
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("XX001"));
            }
            let allocation = allocation_counter::measure(|| drop(reader));
            assert_eq!(allocation.count_total, 0);
            assert_eq!((native.used(), work.used()), (29, 0));
        }
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_scalar_copy_bounds_wide_work_and_preserves_admission() {
    let fixture = source(ScalarExpr::Row((0..2048).map(|_| text(128)).collect()));
    let work = MemoryBudget::new(1 << 20);
    let native = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let (value, polls) = complete(&mut input(fixture.clone(), &native, &work, &token, &token));
    assert!(polls > 8192);
    drop(value);
    for bytes in [0, 1, 4096, 32768, 131_072] {
        let native = MemoryBudget::new(bytes);
        let error = input(fixture.clone(), &native, &work, &token, &token)
            .read(&mut std::io::empty())
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((native.used(), work.used()), (0, 0));
        assert!(native.peak() <= bytes);
    }
}

#[test]
fn scalar_copy_retires_deep_children_on_late_metadata_failure_with_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut deep = text(97);
            for _ in 0..8192 {
                deep = ScalarExpr::Not(Box::new(deep));
            }
            let fixture = source(ScalarExpr::Cast {
                expr: Box::new(deep),
                ty: "t".repeat(32 << 20),
                implicit: false,
            });
            let native = MemoryBudget::new(16 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let sibling = native.reserve(29).unwrap();
            let control = ProductionControl::new(&native, &token, &token);
            for mode in 0..3 {
                let error = if mode == 2 {
                    input(fixture.clone(), &native, &work, &token, &token)
                        .read(&mut std::io::empty())
                        .map(drop)
                } else {
                    let error = if mode == 0 {
                        fixture.clone_with_control(&control).map(drop)
                    } else {
                        fixture.copy_for_rewrite(&control).map(drop)
                    }
                    .unwrap_err();
                    let CatalogRetentionError::Memory(uqa_core::memory::MemoryError::Limit {
                        required,
                        ..
                    }) = error
                    else {
                        panic!("late type-name admission error");
                    };
                    assert!(
                        required >= 32 << 20,
                        "must complete the deep child before type-name admission"
                    );
                    Err(SQLError::from(error))
                }
                .unwrap_err();
                assert_eq!(error.sqlstate(), Some("53200"));
                assert_eq!((native.used(), work.used()), (29, 0));
            }
            drop(sibling);
            assert_eq!(native.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
