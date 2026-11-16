//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ir::{RetainedConstantValue, RetainedScalarLiteral},
    plan::{
        retirement::{construction::Context, empty_query, tests::ScalarSource},
        RelationalPlan,
    },
    ScalarExpr,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

fn tree(
    depth: usize,
    context: &Context,
    native: &MemoryBudget,
    control: &ProductionControl<'_>,
) -> (super::super::Prefix<QueryPlan>, Arc<AtomicUsize>) {
    let retired = Arc::new(AtomicUsize::new(0));
    let mut depth_owner = context.clone();
    let mut query = None;
    for _ in 0..=depth {
        depth_owner = depth_owner.child(control).unwrap();
        let value = RetainedConstantValue::new(
            Arc::new(ScalarSource {
                native: native.clone(),
                minimum: Arc::new(AtomicUsize::new(37)),
                retired: retired.clone(),
            }),
            0,
        );
        let mut parent = empty_query();
        parent.root = RelationalPlan::Values {
            rows: vec![vec![ScalarExpr::RetainedLiteral(
                RetainedScalarLiteral::new(value, None, None, None),
            )]],
            subqueries: query.into_iter().collect(),
        };
        query = Some(parent);
    }
    (context.retain(query.unwrap()), retired)
}

fn input(
    mut retirement: Retirement,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<(), SQLError> {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&native, &original, work.cancellation);
        retirement.retire_from(&control, &mut work).await
    })
    .unwrap()
}

fn advance(input: &mut ImageDecoder<(), SQLError>) -> Result<Option<()>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn suspended_query_tree_retirement_keeps_original_parent_cleanup_available() {
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let sibling = native.reserve(37).unwrap();
    let context = Context::new(&control).unwrap();
    let (query, retired) = tree(64, &context, &native, &control);
    let (parent, parent_retired) = tree(16, &context, &native, &control);
    let mut retirement = context.prepare_retirement(&control).unwrap();
    retirement.query(query.take());
    let mut input = input(retirement, &native, &work, &token, &token);
    assert!(advance(&mut input).unwrap().is_none());
    assert_eq!(allocation_counter::measure(|| drop(parent)).count_total, 0);
    assert_eq!(parent_retired.load(Ordering::Relaxed), 17);
    assert_eq!(retired.load(Ordering::Relaxed), 0);
    let exhausted = native.reserve(native.limit() - native.used()).unwrap();
    let mut polls = 0;
    loop {
        let before = retired.load(Ordering::Relaxed);
        let mut ready = None;
        assert_eq!(
            allocation_counter::measure(|| ready = advance(&mut input).unwrap()).count_total,
            0
        );
        assert!(retired.load(Ordering::Relaxed) - before <= 1);
        polls += 1;
        if ready.is_some() {
            break;
        }
    }
    assert!(polls > 64);
    assert_eq!(retired.load(Ordering::Relaxed), 65);
    drop((input, context, exhausted));
    assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
}

#[test]
fn query_tree_retirement_releases_every_interrupted_prefix_without_new_allocations() {
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let sibling = native.reserve(37).unwrap();
    let mut polls = 0;
    for prefix in 0..100 {
        for cancel in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&native, &original, &invoking);
            let context = Context::new(&control).unwrap();
            let (query, retired) = tree(6, &context, &native, &control);
            let mut retirement = context.prepare_retirement(&control).unwrap();
            retirement.query(query.take());
            let mut input = input(retirement, &native, &work, &original, &invoking);
            let mut complete = false;
            for _ in 0..prefix {
                complete = advance(&mut input).unwrap().is_some();
                if complete {
                    break;
                }
            }
            if complete {
                polls = prefix;
            } else if cancel != 0 {
                if cancel == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut input).unwrap_err().sqlstate(), Some("57014"));
            }
            assert_eq!(allocation_counter::measure(|| drop(input)).count_total, 0);
            assert_eq!(retired.load(Ordering::Relaxed), 7);
            drop(context);
            assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
        }
        if polls != 0 {
            break;
        }
    }
    assert!(polls > 6 && polls < 100);
}

#[test]
fn deep_query_tree_retirement_reuses_admitted_frames_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(128 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let sibling = native.reserve(37).unwrap();
            let context = Context::new(&control).unwrap();
            let (query, retired) = tree(8192, &context, &native, &control);
            let mut retirement = context.prepare_retirement(&control).unwrap();
            retirement.query(query.take());
            let mut input = input(retirement, &native, &work, &token, &token);
            let exhausted = native.reserve(native.limit() - native.used()).unwrap();
            let mut polls = 0;
            loop {
                let ready = advance(&mut input).unwrap();
                polls += 1;
                if ready.is_some() {
                    break;
                }
                assert!(polls < 100_000);
            }
            assert!(polls > 8192);
            assert_eq!(retired.load(Ordering::Relaxed), 8193);
            assert_eq!(allocation_counter::measure(|| drop(input)).count_total, 0);
            drop((context, exhausted));
            assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn query_retirement_admission_failure_preserves_the_original_tree() {
    let native = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let sibling = native.reserve(37).unwrap();
    let context = Context::new(&control).unwrap();
    let (query, retired) = tree(6, &context, &native, &control);
    let exhausted = native.reserve(native.limit() - native.used()).unwrap();
    let error = context.prepare_retirement(&control).err().unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(retired.load(Ordering::Relaxed), 0);
    assert_eq!(allocation_counter::measure(|| drop(query)).count_total, 0);
    assert_eq!(retired.load(Ordering::Relaxed), 7);
    drop((context, exhausted));
    assert_eq!(native.used(), sibling.bytes());
}

#[test]
fn query_tree_retirement_suspends_deep_scalar_roots_and_empty_rows() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(8 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let sibling = native.reserve(37).unwrap();
            let context = Context::new(&control).unwrap().child(&control).unwrap();
            let (mut query, retired) = tree(0, &context, &native, &control);
            let RelationalPlan::Values { rows, .. } = &mut query.root else {
                panic!("values");
            };
            let mut scalar = rows[0].pop().unwrap();
            for _ in 0..8192 {
                scalar = ScalarExpr::Not(Box::new(scalar));
            }
            rows.clear();
            rows.extend((0..4096).map(|_| Vec::new()));
            rows.push(vec![scalar]);
            let mut retirement = context.prepare_retirement(&control).unwrap();
            retirement.query(query.take());
            let mut input = input(retirement, &native, &work, &token, &token);
            let exhausted = native.reserve(native.limit() - native.used()).unwrap();
            let mut first_release = None;
            let mut polls = 0;
            loop {
                let mut ready = None;
                assert_eq!(
                    allocation_counter::measure(|| ready = advance(&mut input).unwrap())
                        .count_total,
                    0
                );
                polls += 1;
                if retired.load(Ordering::Relaxed) != 0 {
                    first_release.get_or_insert(polls);
                }
                if ready.is_some() {
                    break;
                }
                assert!(polls < 100_000);
            }
            assert!(first_release.unwrap() > 8192 + 4096);
            assert_eq!(retired.load(Ordering::Relaxed), 1);
            drop((input, context, exhausted));
            assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
        })
        .unwrap()
        .join()
        .unwrap();
}
