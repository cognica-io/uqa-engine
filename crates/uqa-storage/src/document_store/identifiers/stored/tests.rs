//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::Budgeted,
    CancellationToken,
};

const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

fn decoder(
    value: &str,
    control: &StorageReadControl,
    invoking: &CancellationToken,
) -> ImageDecoder<Option<u128>, StorageBackendError> {
    let value = value.to_owned();
    let reservation = control.memory().reserve(value.capacity()).unwrap();
    let value = Budgeted::new(value, reservation);
    let memory = control.memory().clone();
    let control = control.clone();
    ImageDecoder::new(&memory, invoking, move |mut input| async move {
        parse(&value, "public.한글", &control, &mut input.input()).await
    })
    .unwrap()
}

#[test]
fn stored_document_reservations_match_full_width_unsigned_text_and_errors() {
    let mut inputs = vec![
        String::new(),
        "0".into(),
        "+0".into(),
        "00".into(),
        "00001".into(),
        u64::MAX.to_string(),
        (u128::from(u64::MAX) + 1).to_string(),
        u128::MAX.to_string(),
        "340282366920938463463374607431768211456".into(),
        "-0".into(),
        "-1".into(),
        " 1".into(),
        "1 ".into(),
        "1.0".into(),
        "1e1".into(),
        "+".into(),
        "++1".into(),
        "1\0".into(),
        "１２".into(),
        "1_000".into(),
        "999999999999999999999999999999999999999x".into(),
    ];
    for prefix in ["", "+"] {
        for zeros in [0, 38, 39, 256, 4096] {
            for suffix in [
                "0",
                "7",
                "x",
                "340282366920938463463374607431768211455",
                "340282366920938463463374607431768211456",
                "3402823669209384634633746074317682114550x",
                "340282366920938463463374607431768211455x",
            ] {
                inputs.push(format!("{prefix}{}{suffix}", "0".repeat(zeros)));
            }
        }
    }
    for value in inputs {
        let control = StorageReadControl::with_limit(65536);
        let actual =
            decoder(&value, &control, &CancellationToken::new()).read(&mut std::io::empty());
        let expected = if value.is_empty() {
            Ok(None)
        } else {
            value.parse::<u128>().map(Some)
        };
        match (actual, expected) {
            (Ok(actual), Ok(expected)) => assert_eq!(actual, expected, "{value:?}"),
            (Err(StorageBackendError::Other(actual)), Err(expected)) => assert_eq!(
                actual,
                format!("invalid persisted next id for table `public.한글`: {expected}"),
                "{value:?}"
            ),
            (actual, expected) => {
                panic!("different reservation for {value:?}: {actual:?} / {expected:?}")
            }
        }
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn stored_document_reservations_yield_and_retire_at_every_pending_boundary() {
    let value = format!("+{}18446744073709551616", "0".repeat(4096));
    let control = StorageReadControl::with_limit(65536);
    let mut input = decoder(&value, &control, &CancellationToken::new());
    let mut pending = 0;
    loop {
        let mut result = None;
        let allocations = allocation_counter::measure(|| {
            result = Some(input.advance(&mut std::io::empty(), WINDOW).unwrap());
        });
        assert_eq!(allocations.bytes_max, 0);
        if let Some(result) = result.unwrap() {
            assert_eq!(result, Some(u128::from(u64::MAX) + 1));
            break;
        }
        pending += 1;
    }
    assert!(pending >= value.len() / 256);
    assert_eq!(control.memory().used(), 0);
    for stop in 0..pending {
        for failure in 0..4 {
            let control = StorageReadControl::with_limit(65536);
            let invoking = CancellationToken::new();
            let mut input = decoder(&value, &control, &invoking);
            for _ in 0..stop {
                assert!(input
                    .advance(&mut std::io::empty(), WINDOW)
                    .unwrap()
                    .is_none());
            }
            match failure {
                0 | 1 => {
                    if failure == 0 {
                        invoking.cancel();
                    } else {
                        control.cancellation().cancel();
                    }
                    assert!(matches!(
                        input.advance(&mut std::io::empty(), WINDOW),
                        Err(StorageBackendError::Cancelled(_))
                    ));
                    assert!(input.advance(&mut std::io::empty(), WINDOW).is_err());
                }
                2 => {
                    assert!(input
                        .advance(
                            &mut std::io::empty(),
                            ImageDecodeWindow { bytes: 1, steps: 0 }
                        )
                        .is_err());
                }
                3 => (),
                _ => unreachable!(),
            }
            drop(input);
            assert_eq!(control.memory().used(), 0);
        }
    }
}
