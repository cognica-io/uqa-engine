//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::SQLError;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

fn nested<'a>(
    depth: usize,
    control: ProductionControl<'a>,
    mut work: Work<'a>,
    completed: Arc<AtomicUsize>,
) -> Result<Read<'a, Result<usize>>> {
    Read::new(control, async move {
        work.checkpoint().await;
        control.check()?;
        if depth == 0 {
            completed.fetch_add(1, Ordering::Relaxed);
            return Ok(0);
        }
        let child_completed = Arc::clone(&completed);
        let output = work.submit(move |work| async move {
            nested(depth - 1, control, work, child_completed)?.await
        })?;
        let value = receive(output).await? + 1;
        completed.fetch_add(1, Ordering::Relaxed);
        Ok(value)
    })
}

fn input(
    depth: usize,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<usize, SQLError> {
    counted_input(
        depth,
        native,
        work,
        original,
        invoking,
        Arc::new(AtomicUsize::new(0)),
    )
}

fn counted_input(
    depth: usize,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    completed: Arc<AtomicUsize>,
) -> ImageDecoder<usize, SQLError> {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        let mut driver = Driver::new(control)?;
        let output = driver
            .work()
            .submit(move |work| async move { nested(depth, control, work, completed)?.await })?;
        while driver.advance().await? {
            input.checkpoint().await?;
        }
        Ok(receive(output).await?)
    })
    .unwrap()
}

fn advance(
    input: &mut ImageDecoder<usize, SQLError>,
) -> std::result::Result<Option<usize>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn native_construction_driver_keeps_deep_polling_and_retirement_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(16 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let completed = Arc::new(AtomicUsize::new(0));
            let mut decoder =
                counted_input(8192, &native, &work, &token, &token, Arc::clone(&completed));
            let mut polls = 0;
            loop {
                polls += 1;
                let before = completed.load(Ordering::Relaxed);
                let progress = advance(&mut decoder).unwrap();
                assert!(
                    completed.load(Ordering::Relaxed) - before <= 1,
                    "one poll drained completed parents"
                );
                if let Some(depth) = progress {
                    assert_eq!(depth, 8192);
                    break;
                }
                assert!(polls < 100_000);
            }
            assert!(polls > 3 * 8192);
            assert_eq!(completed.load(Ordering::Relaxed), 8193);
            assert_eq!((native.used(), work.used()), (0, 0));
            let mut decoder = input(8192, &native, &work, &token, &token);
            for _ in 0..8192 {
                assert!(advance(&mut decoder).unwrap().is_none());
            }
            let retired = allocation_counter::measure(|| drop(decoder));
            assert_eq!(retired.count_total, 0);
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_construction_driver_retires_each_prefix_both_cancellations_and_denied_children() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let sibling = native.reserve(43).unwrap();
    for prefix in 0..32 {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut decoder = input(64, &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut decoder).unwrap().is_none());
            }
            if cancellation > 0 {
                if cancellation == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut decoder).unwrap_err().sqlstate(), Some("57014"));
            }
            drop(decoder);
            assert_eq!((native.used(), work.used()), (43, 0));
        }
    }
    let token = CancellationToken::new();
    for capacity in [0, 1, 128, 4096] {
        let limited = MemoryBudget::new(capacity);
        assert_eq!(
            input(64, &limited, &work, &token, &token)
                .read(&mut std::io::empty())
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!((limited.used(), work.used()), (0, 0));
        assert!(limited.peak() <= capacity);
    }
    drop(sibling);
}
