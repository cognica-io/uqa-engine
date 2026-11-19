//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError};

fn nested(depth: usize) -> Value {
    let mut value = Value::Str("leaf".into());
    for _ in 0..depth {
        value = Value::List(vec![value]);
    }
    value
}

fn source(kind: usize, depth: usize) -> OwnedValue {
    let value = nested(depth);
    OwnedValue::new(match kind {
        0 => Value::List(vec![value, Value::Str("later".into())]),
        1 => Value::Map([("first".into(), value), ("last".into(), Value::Int(2))].into()),
        2 => Value::Row(vec![value].into()),
        3 => Value::Record(vec![("field".into(), value)].into()),
        4 => Value::Array(ArrayValue::try_new(vec![Value::Row(vec![value].into())]).unwrap()),
        _ => unreachable!(),
    })
}

#[test]
fn native_value_copy_retires_deep_completed_children_at_late_cancellation_boundaries() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let memory = MemoryBudget::new(64 << 20);
            let sibling = memory.reserve(37).unwrap();
            for kind in 0..5 {
                let source = source(kind, 8192);
                let mut checks = 0_usize;
                let copied = source
                    .clone_budgeted_with_check(&memory, || {
                        checks += 1;
                        Ok(())
                    })
                    .unwrap();
                drop(copied.into_owned_value());
                assert_eq!(memory.used(), sibling.bytes());
                for failure in checks.saturating_sub(12)..=checks {
                    let mut at = 0;
                    let allocation = allocation_counter::measure(|| {
                        let token = CancellationToken::new();
                        let error = source
                            .clone_budgeted_with_check(&memory, || {
                                at += 1;
                                if at == failure {
                                    token.cancel();
                                }
                                token.check()
                            })
                            .map(Budgeted::into_owned_value)
                            .unwrap_err();
                        assert!(matches!(error, ValueRetentionError::Cancelled(_)));
                    });
                    assert_eq!(allocation.count_current, 0);
                    assert_eq!(allocation.bytes_current, 0);
                    assert_eq!(memory.used(), sibling.bytes());
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

fn input(
    value: OwnedValue,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<Produced<OwnedValue>> {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        value
            .clone_from_input(&control, &mut input)
            .await
            .map(Produced::into_owned_value)
            .map_err(|error| match error {
                ValueRetentionError::Memory(error) => ImageIoError::Memory(error),
                ValueRetentionError::Cancelled(error) => ImageIoError::Cancelled(error),
                error @ ValueRetentionError::Malformed { .. } => {
                    panic!("unexpected copy error: {error}")
                }
            })
    })
    .unwrap()
}

fn advance(
    input: &mut ImageDecoder<Produced<OwnedValue>>,
) -> Result<Option<Produced<OwnedValue>>, ImageIoError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn native_value_copy_retires_every_pending_prefix_with_original_ownership() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let sibling = native.reserve(37).unwrap();
    for kind in 0..5 {
        let mut prefixes = 0;
        for prefix in 0..128 {
            let mut complete = false;
            for failure in ["drop", "original", "invoking", "memory"] {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let mut input = input(source(kind, 7), &native, &work, &original, &invoking);
                for _ in 0..prefix {
                    if let Some(value) = advance(&mut input).unwrap() {
                        drop(value);
                        complete = true;
                        break;
                    }
                }
                if !complete {
                    let exhausted = native.reserve(native.available()).unwrap();
                    match failure {
                        "original" => original.cancel(),
                        "invoking" => invoking.cancel(),
                        _ => (),
                    }
                    if failure == "original" || failure == "invoking" {
                        let result = advance(&mut input);
                        assert!(
                            matches!(result, Err(ImageIoError::Cancelled(_))),
                            "{kind}/{prefix}/{failure}: {result:?}"
                        );
                    } else if failure == "memory" {
                        loop {
                            match advance(&mut input) {
                                Ok(None) => (),
                                Ok(Some(value)) => {
                                    drop(value);
                                    break;
                                }
                                Err(ImageIoError::Memory(_)) => break,
                                Err(error) => panic!("unexpected error: {error}"),
                            }
                        }
                    }
                    assert_eq!(allocation_counter::measure(|| drop(input)).count_total, 0);
                    drop(exhausted);
                }
                assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
            }
            prefixes += 1;
            if complete {
                break;
            }
        }
        assert!(prefixes > 7 && prefixes < 128);
    }
}

#[test]
fn native_value_copy_and_external_admission_share_iterative_private_retirement() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let source = source(3, 8192);
            let ordinary = ProductionControl::uncontrolled()
                .copy_value(&source)
                .unwrap();
            assert_eq!(ordinary.reserved_bytes(), 0);
            drop(ordinary.into_owned_value());

            let memory = MemoryBudget::new(64);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            assert!(matches!(
                control.retain_external_value(nested(8192)),
                Err(ValueRetentionError::Memory(_))
            ));
            assert_eq!(memory.used(), 0);

            let memory = MemoryBudget::new(64 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let result = Value::Int(1).transform_with_control(&control, |_| {
                let value = control.retain_external_value(nested(8192))?;
                token.cancel();
                Ok::<_, ValueRetentionError>(Some(value))
            });
            assert!(matches!(result, Err(ValueRetentionError::Cancelled(_))));
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
