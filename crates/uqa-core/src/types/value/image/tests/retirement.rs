//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    image_io::ImageDecodeWindow, memory::ProductionControl, types::OwnedValue, ValueRetirement,
};

fn nested(depth: usize) -> Value {
    let mut value = Value::Int(7);
    for _ in 0..depth {
        value = Value::List(vec![value]);
    }
    value
}

fn source() -> OwnedValue {
    OwnedValue::new(Value::Map(
        [
            (
                "record".into(),
                Value::Record(
                    vec![
                        ("first".into(), nested(32)),
                        ("later".into(), Value::Str("payload".into())),
                    ]
                    .into(),
                ),
            ),
            (
                "row".into(),
                Value::Row(
                    RowValue::typed(
                        vec![nested(32)],
                        vec![RecordFieldType {
                            oid: 25,
                            type_modifier: -1,
                            collation_oid: Some(100),
                        }],
                    )
                    .unwrap(),
                ),
            ),
        ]
        .into(),
    ))
}

#[test]
fn native_value_image_retirement_preserves_leases_at_every_pending_prefix() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let source = source();
            let bytes = encode(&source);
            let memory = MemoryBudget::new(1 << 20);
            let sibling = memory.reserve(37).unwrap();
            let window = ImageDecodeWindow { bytes: 7, steps: 1 };
            let mut completed = false;
            for prefix in 0..2048 {
                let mut complete = false;
                for failure in ["drop", "cancel", "memory"] {
                    let token = CancellationToken::new();
                    let mut input = ValueImageInput::new(&memory, &token).unwrap();
                    let mut unread = bytes.as_slice();
                    for _ in 0..prefix {
                        if let Some(value) = input.advance(&mut unread, window).unwrap() {
                            assert_eq!(*value, *source);
                            drop(value.into_owned_value());
                            complete = true;
                            break;
                        }
                    }
                    if !complete {
                        let exhausted = memory.reserve(memory.available()).unwrap();
                        if failure == "cancel" {
                            token.cancel();
                            assert!(matches!(
                                input.advance(&mut unread, window),
                                Err(ValueImageError::Cancelled(_))
                            ));
                        } else if failure == "memory" {
                            loop {
                                match input.advance(&mut unread, window) {
                                    Ok(None) => (),
                                    Ok(Some(value)) => {
                                        assert_eq!(*value, *source);
                                        drop(value.into_owned_value());
                                        break;
                                    }
                                    Err(ValueImageError::Memory(_)) => break,
                                    Err(error) => panic!("unexpected image failure: {error}"),
                                }
                            }
                        }
                        assert_eq!(allocation_counter::measure(|| drop(input)).count_total, 0);
                        drop(exhausted);
                    }
                    assert_eq!(memory.used(), sibling.bytes());
                }
                if complete {
                    assert!(prefix > 64);
                    completed = true;
                    break;
                }
            }
            assert!(completed);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_value_image_retires_completed_children_after_metadata_and_trailing_errors() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let source = source();
            let mut bytes = encode(&source);
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let sibling = memory.reserve(37).unwrap();
            for length in bytes.len().saturating_sub(24)..bytes.len() {
                assert!(matches!(
                    decode_value_image(&bytes[..length], &memory, &token),
                    Err(ValueImageError::Io(_))
                ));
                assert_eq!(memory.used(), sibling.bytes());
            }
            bytes.push(1);
            assert!(matches!(
                decode_value_image(&bytes, &memory, &token),
                Err(ValueImageError::Invalid("trailing bytes"))
            ));
            assert_eq!(memory.used(), sibling.bytes());

            // Invalid legacy elements reach the existing array validator with an owned deep child.
            let array = ArrayValue::try_new(vec![Value::Row(vec![nested(8192)].into())]).unwrap();
            let control = ProductionControl::uncontrolled();
            let owned = control.finish(array, None).unwrap();
            let released = allocation_counter::measure(|| {
                assert!(LegacyVectorValue::try_from_array_with_control(
                    LegacyVectorKind::Oid,
                    owned,
                    &control
                )
                .unwrap()
                .is_none());
            });
            assert_eq!(released.count_total, 0);
            assert!(released.count_current < -8192);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_value_image_preserves_the_existing_nesting_limit() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let allowed = OwnedValue::new(nested(MAX_DEPTH));
    let bytes = encode(&allowed);
    let restored = decode_value_image(&bytes, &memory, &token)
        .unwrap()
        .into_owned_value();
    assert_eq!(encode(&restored), bytes);
    drop(restored);
    assert_eq!(memory.used(), 0);
    let rejected = nested(MAX_DEPTH + 1);
    assert!(matches!(
        write_value_image(&rejected, &mut Vec::new(), &memory, &token),
        Err(ValueImageError::Invalid("nesting depth"))
    ));
    drop(ValueRetirement::new(rejected));
    assert_eq!(memory.used(), 0);
}
