//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    plan::{QueryPlan, RelationalPlan},
    ScalarExpr,
};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken, Value,
};

type Input = ImageDecoder<ResolvedWindows, SQLError>;

fn fixture() -> Pending<QueryBlockPlan> {
    let crate::Statement::Select(statement) = crate::compile("SELECT row_number() OVER framed WINDOW base AS (PARTITION BY 1), ordered AS (base ORDER BY 2 DESC NULLS LAST), framed AS (ordered ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING)").unwrap().remove(0) else { panic!("SELECT") };
    let RelationalPlan::QueryBlock(block) = QueryPlan::lower(*statement).root else {
        panic!("block")
    };
    Pending::new(*block)
}

fn input(
    source: Arc<Pending<QueryBlockPlan>>,
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
        resolved_window_definitions_from(&source, &control, &mut input).await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<ResolvedWindows>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> (ResolvedWindows, usize) {
    for polls in 1..1_000_000 {
        if let Some(value) = advance(input).unwrap() {
            return (value, polls);
        }
    }
    panic!("canonical window resolution stopped progressing")
}

#[test]
fn retained_canonical_windows_preserve_inherited_clauses_and_exact_owners() {
    let source = Arc::new(fixture());
    let native = MemoryBudget::new(4 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let ordinary = resolved_window_definitions_owned(&source, &control).unwrap();
    let (resumed, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    assert!(polls > 50);
    assert_eq!(&*ordinary, &*resumed);
    assert_eq!(
        ordinary.memory.as_ref().unwrap().bytes(),
        resumed.memory.as_ref().unwrap().bytes()
    );
    assert_eq!(resumed.len(), 3);
    for (slot, spec) in resumed.iter().enumerate() {
        assert_eq!(spec.definition, Some(slot));
        assert_eq!(spec.partition_by, [ScalarExpr::Literal(Value::Int(1))]);
        assert_eq!(spec.order_by.len(), usize::from(slot != 0));
        if slot != 0 {
            assert_eq!(spec.order_by, source.windows[1].spec.order_by);
            assert!(spec.order_by[0].descending);
        }
        assert_eq!(spec.frame, source.windows[slot].spec.frame);
    }
    drop((ordinary, resumed));
    assert_eq!((native.used(), work.used()), (0, 0));
}

#[test]
fn canonical_window_validation_keeps_first_failure_order_across_work_boundaries() {
    for case in 0..8 {
        let mut source = fixture();
        let expected = match case {
            0 => {
                source.windows[0].spec.definition = Some(0);
                source.windows[0].name = Some(String::new());
                "a canonical specification refers to a call slot"
            }
            1 => {
                source.windows[1].name = source.windows[0].name.clone();
                source.windows[1].inherited = Some(999);
                "a canonical window name is empty or repeated"
            }
            2 => {
                source.windows[0].name = Some(String::new());
                "a canonical window name is empty or repeated"
            }
            3 => {
                source.windows[0].inherited = Some(1);
                "an inherited window is not an earlier named definition"
            }
            4 => {
                source.windows[0].name = None;
                "an inherited window is not an earlier named definition"
            }
            5 => {
                source.windows[1].spec.partition_by.push(ScalarExpr::Star);
                "a canonical window has incompatible inherited clauses"
            }
            6 => {
                source.windows[0].spec.order_by = source.windows[1].spec.order_by.clone();
                "a canonical window has incompatible inherited clauses"
            }
            7 => {
                source.windows[0].spec.frame = source.windows[2].spec.frame.clone();
                "a canonical window has incompatible inherited clauses"
            }
            _ => unreachable!(),
        };
        let source = Arc::new(source);
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let ordinary = resolved_window_definitions_owned(
            &source,
            &ProductionControl::new(&native, &token, &token),
        )
        .err()
        .expect("invalid canonical definition");
        let resumed = input(source, &native, &work, &token, &token)
            .read(&mut std::io::empty())
            .err()
            .expect("invalid retained definition");
        assert!(ordinary.to_string().contains(expected), "{ordinary}");
        assert_eq!(ordinary.to_string(), resumed.to_string());
        assert_eq!(ordinary.sqlstate(), resumed.sqlstate());
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_canonical_windows_retire_every_interrupted_resolution_and_freeze_prefix() {
    let source = Arc::new(fixture());
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
                assert_eq!(
                    advance(&mut reader)
                        .err()
                        .expect("cancelled resolution")
                        .sqlstate(),
                    Some("57014")
                );
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
fn canonical_window_names_use_bounded_borrowed_comparisons() {
    let mut source = fixture();
    let prefix = "é한🙂".repeat(32_768);
    source.windows[0].name = Some(format!("{prefix}a"));
    source.windows[1].name = Some(format!("{prefix}b"));
    let source = Arc::new(source);
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (value, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    assert!(polls > 2 * prefix.len() / 4096);
    assert!(
        native.peak() < prefix.len(),
        "name payloads remain borrowed"
    );
    let required = native.peak();
    assert!(required > 4);
    drop(value);
    for bytes in [0, 1, required / 4, required / 2, required - 1] {
        let native = MemoryBudget::new(bytes);
        let control = ProductionControl::new(&native, &token, &token);
        let ordinary = resolved_window_definitions_owned(&source, &control)
            .err()
            .expect("denied canonical allocation");
        let resumed = input(source.clone(), &native, &work, &token, &token)
            .read(&mut std::io::empty())
            .err()
            .expect("denied retained allocation");
        assert_eq!(ordinary.to_string(), resumed.to_string());
        assert_eq!(resumed.sqlstate(), Some("53200"));
        assert_eq!((native.used(), work.used()), (0, 0));
        assert!(native.peak() <= bytes);
    }
}

#[test]
fn canonical_window_failure_retires_completed_deep_definitions_with_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut source = fixture();
            let mut deep = ScalarExpr::Literal(Value::Str("leaf".into()));
            for _ in 0..8192 {
                deep = ScalarExpr::Not(Box::new(deep));
            }
            source.windows[0].spec.partition_by = vec![deep];
            source.windows[1].name = source.windows[0].name.clone();
            let source = Arc::new(source);
            let native = MemoryBudget::new(16 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let ordinary = resolved_window_definitions_owned(
                &source,
                &ProductionControl::new(&native, &token, &token),
            )
            .err()
            .expect("late duplicate");
            assert!(ordinary.to_string().contains("repeated"));
            let resumed = input(source, &native, &work, &token, &token)
                .read(&mut std::io::empty())
                .err()
                .expect("late retained duplicate");
            assert_eq!(ordinary.to_string(), resumed.to_string());
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}
