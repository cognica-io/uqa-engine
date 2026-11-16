//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::ExpressionPlan;
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken, Value,
};

type Input = ImageDecoder<OwnedScalarExpr, SQLError>;

fn expression(sql: &str) -> Expr {
    let crate::ast::Statement::Select(mut query) = crate::compile(sql).unwrap().remove(0) else {
        panic!("SELECT expression");
    };
    query.projections.remove(0).expr
}

fn input(
    source: Arc<Expr>,
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
        ExpressionPlan::lower_scalar_from_input(&source, &control, &mut input).await
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
    for polls in 1..1_000_000 {
        if let Some(value) = advance(input).unwrap() {
            return (value, polls);
        }
    }
    panic!("native scalar constructor stopped progressing")
}

#[test]
fn retained_scalar_lowering_preserves_all_containers_windows_and_movable_child_ownership() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    for sql in [
        "SELECT a", "SELECT t.a", "SELECT 'literal'", "SELECT $1", "SELECT concat(a, '한🙂')",
        "SELECT ARRAY[1,2,3]", "SELECT ROW(a,'text')", "SELECT a+1", "SELECT -a",
        "SELECT NOT a", "SELECT a AND b AND c", "SELECT a OR b OR c", "SELECT a IS NOT NULL",
        "SELECT a BETWEEN 1 AND 2", "SELECT a NOT IN (1,2,3)",
        "SELECT CASE a WHEN 1 THEN 'first' ELSE 'last' END", "SELECT CASE WHEN a THEN 'yes' END",
        "SELECT CAST(a AS text)", "SELECT sum(a ORDER BY b DESC NULLS FIRST) FILTER (WHERE c)",
        "SELECT sum(a) FILTER (WHERE d) OVER (PARTITION BY b ORDER BY c ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING)",
        "SELECT sum(a) OVER (ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)",
        "SELECT sum(a) OVER (ROWS BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING)",
    ] {
        let source = Arc::new(expression(sql));
        let ordinary = ExpressionPlan::lower_scalar_for_rewrite(&source, &control).unwrap();
        let (value, polls) = complete(&mut input(source, &native, &work, &token, &token));
        assert!(polls > 1, "{sql}");
        assert_eq!(*value, *ordinary, "{sql}");
        assert_eq!(value.reserved_bytes(), ordinary.reserved_bytes(), "{sql}");
        assert_eq!(work.used(), 0);
        let value = value.map_children(&control, |_, child| Ok::<_, SQLError>(child)).unwrap();
        assert_eq!(*value, *ordinary);
        assert_eq!(native.used(), value.reserved_bytes() + ordinary.reserved_bytes());
        drop((value, ordinary));
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn retained_scalar_lowering_retires_every_prefix_and_both_cancellation_owners() {
    let source = Arc::new(Expr::Row(vec![
        expression("SELECT CASE WHEN a THEN concat(b, '한🙂') ELSE 'end' END"),
        Expr::Literal(Value::Str("payload_é🦀".repeat(1024))),
    ]));
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (value, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    drop(value);
    let sibling = native.reserve(29).unwrap();
    for prefix in 0..polls - 1 {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(source.clone(), &native, &work, &original, &invoking);
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
            drop(reader);
            assert_eq!((native.used(), work.used()), (29, 0));
        }
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_scalar_lowering_keeps_wide_work_bounded_and_releases_discarded_siblings() {
    let source = Arc::new(Expr::Row(
        (0..2048)
            .map(|index| Expr::Literal(Value::Str(format!("item_{index}_{}", "x".repeat(128)))))
            .collect(),
    ));
    let native = MemoryBudget::new(4 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (value, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    assert!(polls > 8192, "{polls}");
    let selected = value.select_child(37);
    let expected = format!("item_37_{}", "x".repeat(128));
    assert_eq!(*selected, ScalarExpr::Literal(Value::Str(expected.clone())));
    assert_eq!(native.used(), expected.len());
    drop(selected);
    for bytes in [0, 1, 4096, 32768, 131_072] {
        let native = MemoryBudget::new(bytes);
        let error = input(source.clone(), &native, &work, &token, &token)
            .read(&mut std::io::empty())
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((native.used(), work.used()), (0, 0));
        assert!(native.peak() <= bytes);
    }
}

#[test]
fn retained_scalar_lowering_uses_constant_poll_stack_for_deep_syntax() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut source = Expr::Literal(Value::Str("leaf".into()));
            for _ in 0..4096 {
                source = Expr::Not(Box::new(source));
            }
            let source = Arc::new(source);
            let native = MemoryBudget::new(16 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let (mut value, polls) =
                complete(&mut input(source.clone(), &native, &work, &token, &token));
            assert!(polls > 8192, "{polls}");
            for _ in 0..4096 {
                value = value.select_child(0);
            }
            assert_eq!(*value, ScalarExpr::Literal(Value::Str("leaf".into())));
            assert_eq!(native.used(), 4);
            drop(value);
            let mut reader = input(source, &native, &work, &token, &token);
            for _ in 0..4096 {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            drop(reader);
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn retained_scalar_lowering_rejects_queries_without_leaking_partial_inputs() {
    for sql in [
        "SELECT (SELECT 1)",
        "SELECT EXISTS (SELECT 1)",
        "SELECT a IN (SELECT 1)",
    ] {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let source = Arc::new(Expr::Row(vec![
            Expr::Literal(Value::Str("retained".into())),
            expression(sql),
        ]));
        let error = input(source, &native, &work, &token, &token)
            .read(&mut std::io::empty())
            .unwrap_err();
        assert!(error.to_string().contains("contains a subquery"));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}
