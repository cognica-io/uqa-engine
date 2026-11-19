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

#[test]
fn retained_future_admission_precedes_construction() {
    for (capacity, cancellation) in [(37, 0), (512, 1), (512, 2), (512, 0)] {
        let memory = MemoryBudget::new(capacity);
        let sibling = memory.reserve(37).unwrap();
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancellation == 1 {
            original.cancel();
        } else if cancellation == 2 {
            invoking.cancel();
        }
        let constructed = AtomicUsize::new(0);
        let control = ProductionControl::new(&memory, &original, &invoking);
        let result = retain_with(control, || {
            constructed.fetch_add(1, Ordering::Relaxed);
            std::future::ready([0_u8; 128])
        });
        if capacity == 512 && cancellation == 0 {
            assert_eq!(constructed.load(Ordering::Relaxed), 1);
            assert!(memory.used() >= 37 + 128);
            assert_eq!(crate::catalog::work::inline(result.unwrap()), [0; 128]);
        } else {
            assert!(result.is_err());
            assert_eq!(constructed.load(Ordering::Relaxed), 0);
        }
        assert_eq!(memory.used(), 37);
        drop(sibling);
    }
}

fn mode_task<'a>(
    depth: usize,
    expected: bool,
    control: ProductionControl<'a>,
    mut work: Work<'a>,
) -> Result<Read<'a, Result<usize>>> {
    use crate::semantics::sets::placement_scope::{placing_set_calls, SetPlacementScope};
    Read::new(control, async move {
        assert_eq!(placing_set_calls(), expected, "task entry at {depth}");
        work.checkpoint().await;
        assert_eq!(placing_set_calls(), expected, "task resumption at {depth}");
        if depth == 0 {
            return Ok(1);
        }
        let output = {
            let _scope = if expected {
                SetPlacementScope::stored_definition()
            } else {
                SetPlacementScope::compiled_source()
            };
            work.submit(move |work| async move {
                mode_task(depth - 1, !expected, control, work)?.await
            })?
        };
        assert_eq!(placing_set_calls(), expected, "after child submission");
        let children = receive(output).await?;
        assert_eq!(placing_set_calls(), expected, "after child completion");
        Ok(children + 1)
    })
}

#[test]
fn native_tasks_preserve_nested_analysis_modes_across_threads_and_work_windows() {
    use crate::semantics::sets::placement_scope::{placing_set_calls, SetPlacementScope};
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut driver = Driver::new(control).unwrap();
    let output = {
        let _scope = SetPlacementScope::stored_definition();
        driver
            .work()
            .submit(move |work| async move { mode_task(12, false, control, work)?.await })
            .unwrap()
    };
    assert!(placing_set_calls());
    let mut finished = false;
    for step in 0..128 {
        let current = step % 2 == 0;
        let (next, advanced) = std::thread::scope(|threads| {
            threads
                .spawn(move || {
                    assert!(placing_set_calls());
                    let scope = if current {
                        SetPlacementScope::compiled_source()
                    } else {
                        SetPlacementScope::stored_definition()
                    };
                    let advanced = crate::catalog::work::inline(driver.advance()).unwrap();
                    assert_eq!(placing_set_calls(), current, "invoking mode at {step}");
                    drop(scope);
                    assert!(placing_set_calls());
                    (driver, advanced)
                })
                .join()
                .unwrap()
        });
        driver = next;
        assert!(placing_set_calls());
        if !advanced {
            finished = true;
            break;
        }
    }
    assert!(finished);
    assert_eq!(crate::catalog::work::inline(receive(output)).unwrap(), 13);
    drop(driver);
    assert_eq!(budget.used(), 0);
}

#[test]
fn native_task_analysis_scopes_restore_the_invoking_thread_after_failure() {
    use crate::semantics::sets::placement_scope::{placing_set_calls, SetPlacementScope};
    for panic in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let sibling = budget.reserve(43).unwrap();
        let mut driver = Driver::new(control).unwrap();
        let output = {
            let _scope = SetPlacementScope::stored_definition();
            driver
                .work()
                .submit(move |mut work| async move {
                    assert!(!placing_set_calls());
                    work.checkpoint().await;
                    assert!(!placing_set_calls());
                    assert!(!panic, "task unwind");
                    Err::<(), _>(SQLError::Internal("selected task failure".into()))
                })
                .unwrap()
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            while crate::catalog::work::inline(driver.advance()).unwrap() {
                assert!(placing_set_calls());
            }
        }));
        assert!(placing_set_calls());
        assert_eq!(result.is_err(), panic);
        if !panic {
            assert!(
                matches!(crate::catalog::work::inline(receive(output.clone())).unwrap_err(), SQLError::Internal(message) if message == "selected task failure")
            );
        }
        drop(output);
        drop(driver);
        assert!(placing_set_calls());
        assert_eq!(budget.used(), 43);
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}
