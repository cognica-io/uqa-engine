//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::StorageBackendError;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    CancellationToken,
};

const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

fn decoder(
    json: &str,
    control: &StorageReadControl,
    invoking: &CancellationToken,
) -> ImageDecoder<StatisticsMaintenance, StorageBackendError> {
    let json = json.to_owned();
    let reservation = control.memory().reserve(json.capacity()).unwrap();
    let json = Budgeted::new(json, reservation);
    let memory = control.memory().clone();
    let control = control.clone();
    ImageDecoder::new(&memory, invoking, move |mut input| async move {
        StatisticsMaintenance::from_stored_from(json, &control, &mut input.input()).await
    })
    .unwrap()
}

fn check(json: &str) {
    let ordinary = serde_json::from_str::<StatisticsMaintenance>(json);
    let control = StorageReadControl::with_limit(1024 * 1024);
    let result = decoder(json, &control, &CancellationToken::new()).read(&mut std::io::empty());
    match (ordinary, result) {
        (Ok(expected), Ok(actual)) => assert_eq!(actual, expected, "{json}"),
        (Err(_), Err(StorageBackendError::Serde(_))) => (),
        (expected, actual) => {
            panic!("different stored state for {json:?}: {expected:?} / {actual:?}")
        }
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn stored_statistics_maintenance_matches_struct_defaults_fields_and_unsigned_values() {
    let complete = StatisticsMaintenance {
        object_id: Some([255; 16]),
        generation: u64::MAX,
        changes: 7,
        dirty_since_ms: 8,
        analyzed_rows: Some(u64::MAX),
        statistics_format: u32::MAX,
        analyzed_at: Some(9),
        analysis_generation: Some(10),
    };
    check(&serde_json::to_string(&complete).unwrap());
    for json in [
        "{}",
        "[]",
        "[null]",
        "[null,1,2,3,4,5,6,7]",
        "[null,1,2,3,null,5,null,null]",
        "{\"changes\":9,\"analyzed_rows\":0}",
        "{\"gene\\u0072ation\":18446744073709551615}",
        "{\"unknown\":[1,{\"ignored\":true}],\"changes\":2}",
        "{\"unknown\":1,\"unknown\":2}",
        "{\"unknown\":\"\\ud800\"}",
        "{\"unknown\":{\"\\ud800\":0}}",
        "{\"unknown\":1e9999999999999999999999999}",
        "null",
        "true",
        "0",
        "\"{}\"",
        "{}{}",
        "[null,1,2,3,4,5,6,7,8]",
        "{\"changes\":null}",
        "{\"changes\":true}",
        "{\"changes\":[]}",
        "{\"changes\":1,\"changes\":2}",
        "{\"changes\":1,\"chang\\u0065s\":2}",
        "{\"changes\":1.0}",
        "{\"changes\":1e0}",
        "{\"changes\":-0}",
        "{\"changes\":18446744073709551616}",
        "{\"statistics_format\":4294967296}",
        "{\"object_id\":[]}",
        "{\"object_id\":\"invalid\"}",
        "{\"object_id\":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,256]}",
        "{\"object_id\":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17]}",
        "{\"\\ud800\":0}",
        "{\"ignored\":\"\\uXX00\"}",
        "{\"changes\":1,}",
    ] {
        check(json);
    }
    for depth in [16, 127, 128, 256] {
        check(&format!(
            "{{\"unknown\":{}0{}}}",
            "[".repeat(depth),
            "]".repeat(depth)
        ));
    }
    let json = serde_json::to_string(&complete).unwrap();
    for end in 0..json.len() {
        check(&json[..end]);
    }
}

#[test]
fn stored_statistics_maintenance_retires_every_pending_cancellation_window_and_drop() {
    let json = r#"{"changes":5,"object_id":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16],"analyzed_rows":7,"unknown":[1,{"ignored":"content"}]}"#;
    let control = StorageReadControl::with_limit(65536);
    let mut original = decoder(json, &control, &CancellationToken::new());
    let mut pending = 0;
    let expected = loop {
        if let Some(value) = original.advance(&mut std::io::empty(), WINDOW).unwrap() {
            break value;
        }
        pending += 1;
    };
    assert!(expected.invalidates_existing_statistics());
    assert!(pending > 16);
    assert_eq!(control.memory().used(), 0);
    for stop in 0..pending {
        for failure in 0..5 {
            let control = StorageReadControl::with_limit(65536);
            let invoking = CancellationToken::new();
            let mut input = decoder(json, &control, &invoking);
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
                            ImageDecodeWindow { bytes: 0, steps: 1 }
                        )
                        .is_err());
                    assert!(input.advance(&mut std::io::empty(), WINDOW).is_err());
                }
                3 => {
                    let used = control.memory().used();
                    let blocker = control.memory().reserve(65536 - used).unwrap();
                    match input.read(&mut std::io::empty()) {
                        Ok(value) => assert_eq!(value, expected),
                        Err(StorageBackendError::Memory(_)) => (),
                        Err(error) => panic!("unexpected error: {error}"),
                    }
                    drop(blocker);
                    assert_eq!(control.memory().used(), 0);
                    continue;
                }
                4 => (),
                _ => unreachable!(),
            }
            drop(input);
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn stored_statistics_maintenance_skips_large_values_in_bounded_polls() {
    let json = format!(
        "{{\"unknown\":\"{}\",\"changes\":5,\"analyzed_rows\":7}}",
        "z".repeat(1024 * 1024)
    );
    let control = StorageReadControl::with_limit(2 * 1024 * 1024);
    let mut input = decoder(&json, &control, &CancellationToken::new());
    let initial = control.memory().used();
    let mut polls = 0;
    loop {
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(input.advance(&mut std::io::empty(), WINDOW).unwrap());
        });
        assert!(
            allocation.bytes_max < 1024,
            "one poll allocated {} bytes",
            allocation.bytes_max
        );
        assert!(control.memory().used() <= initial + 1024);
        polls += 1;
        if let Some(value) = result.unwrap() {
            assert!(value.invalidates_existing_statistics());
            break;
        }
    }
    assert!(polls >= 256);
    assert_eq!(control.memory().used(), 0);
}
