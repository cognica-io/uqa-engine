//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::image_io::{ImageDecodeWindow, ImageDecoder};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("value copying must not read a source")
    }
}

fn input(
    value: Value,
    budget: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<Budgeted<Value>> {
    ImageDecoder::new(budget, token, |mut reader| async move {
        value
            .clone_budgeted_from(&mut reader.input())
            .await
            .map_err(|error| match error {
                ValueRetentionError::Memory(error) => crate::image_io::ImageIoError::Memory(error),
                ValueRetentionError::Cancelled(error) => {
                    crate::image_io::ImageIoError::Cancelled(error)
                }
                error @ ValueRetentionError::Malformed { .. } => {
                    panic!("unexpected copying error: {error}")
                }
            })
    })
    .unwrap()
}

#[test]
fn retained_value_copies_preserve_native_variants_capacities_and_shared_descriptors() {
    let token = CancellationToken::new();
    let mut source = values();
    source.push(Value::Datum(crate::DatumValue::new(25, 7, vec![1, 2, 3])));
    for value in source {
        let ordinary_budget = MemoryBudget::new(8 << 20);
        let ordinary = value.clone_budgeted(&ordinary_budget, &token).unwrap();
        let budget = MemoryBudget::new(8 << 20);
        let mut input = input(value, &budget, &token);
        let mut steps = 0;
        let copied = loop {
            steps += 1;
            if let Some(value) = input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
            {
                break value;
            }
        };
        assert_eq!(*copied, *ordinary);
        assert_eq!(copied.reserved_bytes(), ordinary.reserved_bytes());
        if let (Value::Record(a), Value::Record(b)) = (&*copied, &*ordinary) {
            assert!(a.descriptor().shares_storage_with(b.descriptor()));
        }
        if let Value::Str(text) = &*copied {
            assert!(steps >= text.len().div_ceil(4096));
        }
        drop((copied, input, ordinary));
        assert_eq!(budget.used(), 0);
        assert_eq!(ordinary_budget.used(), 0);
    }
    let budget = MemoryBudget::new(0);
    let allocation = allocation_counter::measure(|| {
        let value = Value::Int(9).clone_budgeted(&budget, &token).unwrap();
        assert_eq!(*value, Value::Int(9));
    });
    assert_eq!(allocation.count_total, 0);
}

#[test]
fn retained_value_copy_failure_retires_each_partially_constructed_native_owner() {
    for failure in ["cancel", "memory", "window", "drop"] {
        let budget = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let value = Value::List(
            (0..64)
                .map(|i| Value::Str(format!("{i}{}", "한🙂".repeat(2048))))
                .collect(),
        );
        let mut input = input(value, &budget, &token);
        for _ in 0..20 {
            assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
                .is_none());
        }
        let held = (failure == "memory").then(|| budget.reserve(budget.available()).unwrap());
        if failure == "cancel" {
            token.cancel();
        }
        if failure != "drop" {
            loop {
                match input.advance(
                    &mut NoRead,
                    ImageDecodeWindow {
                        bytes: 1,
                        steps: usize::from(failure != "window"),
                    },
                ) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("failed copy published: {failure}"),
                    Err(_) => break,
                }
            }
        }
        drop((input, held));
        assert_eq!(budget.used(), 0, "{failure}");
    }
}

fn controlled_input(
    value: Value,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<Produced<Value>> {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        value
            .clone_from_input(&control, &mut input)
            .await
            .map_err(|error| match error {
                ValueRetentionError::Memory(error) => crate::image_io::ImageIoError::Memory(error),
                ValueRetentionError::Cancelled(error) => {
                    crate::image_io::ImageIoError::Cancelled(error)
                }
                error @ ValueRetentionError::Malformed { .. } => {
                    panic!("unexpected copying error: {error}")
                }
            })
    })
    .unwrap()
}

#[test]
fn retained_value_copy_keeps_destination_allowance_separate_from_work_channel() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let source = Value::List(vec![
        Value::Str("é🦀".repeat(4096)),
        Value::Bytes(vec![7; 32768]),
    ]);
    let control = ProductionControl::new(&native, &token, &token);
    let ordinary = control.copy_value(&source).unwrap();
    let copied = controlled_input(source, &native, &work, &token, &token)
        .read(&mut NoRead)
        .unwrap();
    assert_eq!(*copied, *ordinary);
    assert_eq!(copied.reserved_bytes(), ordinary.reserved_bytes());
    assert_eq!(native.used(), copied.reserved_bytes() * 2);
    assert_eq!(work.used(), 0);
    drop((copied, ordinary));
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_value_copy_keeps_both_cancellation_owners_active_inside_large_payloads() {
    for cancel_original in [true, false] {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut input = controlled_input(
            Value::Str("é🦀".repeat(8192)),
            &native,
            &work,
            &original,
            &invoking,
        );
        for _ in 0..5 {
            assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
                .is_none());
        }
        assert!(native.used() > 0);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            input.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 }),
            Err(crate::image_io::ImageIoError::Cancelled(_))
        ));
        drop(input);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}
