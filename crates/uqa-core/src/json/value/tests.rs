//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};
use std::sync::Arc;

fn protocol() -> JsonScalarProtocol {
    JsonScalarProtocol::from_values(
        &serde_json::Number::from(1u8),
        serde_json::value::RawValue::NULL,
    )
    .unwrap()
}

fn oracle(text: &str) -> Result<(Option<u64>, JsonFieldProjection), serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let field = value
        .as_object()
        .map_or(JsonFieldProjection::default(), |fields| {
            JsonFieldProjection {
                present: fields.contains_key("format"),
                foreign: fields.keys().any(|name| name != "format"),
                unsigned: fields.get("format").and_then(serde_json::Value::as_u64),
            }
        });
    Ok((value.as_u64(), field))
}

fn compare(text: &str) {
    let memory = MemoryBudget::new(4 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let actual = super::super::work::inline(observe_json_text(
        text.as_bytes(),
        &control,
        &mut (),
        protocol(),
        Some("format"),
    ))
    .map(|observation| (observation.unsigned(), observation.field()));
    match (oracle(text), actual) {
        (Ok(expected), Ok(actual)) => assert_eq!(actual, expected, "{text}"),
        (Err(_), Err(JsonReadError::InvalidJson)) => (),
        (expected, actual) => panic!("{text}: {expected:?} / {actual:?}"),
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn generic_json_observation_preserves_public_scalars_duplicates_and_overwritten_errors() {
    let protocol = protocol();
    let mut values = [
        "null",
        "true",
        "[]",
        "{}",
        "-0",
        "0",
        "1",
        "-1",
        "1.0",
        "1e0",
        "1e999999",
        "18446744073709551615",
        "18446744073709551616",
        "01",
        "1e",
        "NaN",
        r#""\ud800""#,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    for key in [protocol.number, protocol.raw] {
        for payload in [
            "1",
            "-0",
            "1.0",
            "1e99999",
            " 1",
            "1 ",
            "01",
            "[]",
            "{}",
            r#"{"format":7}"#,
            "null",
            "true",
            r#""\ud800""#,
        ] {
            let text = serde_json::to_string(payload).unwrap();
            values.push(format!(r"{{{key:?}:{text}}}"));
            values.push(format!(r#"{{{key:?}:{text},"format":1}}"#));
            values.push(format!(r#"{{"format":1,{key:?}:{text}}}"#));
            values.push(format!(r"{{{key:?}:null}}"));
        }
    }
    for value in values {
        compare(&value);
        compare(&format!(r#"{{"format":{value}}}"#));
        compare(&format!(r#"{{"format":{value},"format":1}}"#));
        compare(&format!(r#"{{"format":1,"format":{value}}}"#));
        compare(&format!(r#"{{"ignored":{value},"format":1}}"#));
    }
    for depth in [1, 126, 127, 128] {
        compare(&format!("{}0{}", "[".repeat(depth), "]".repeat(depth)));
    }
}

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("borrowed JSON performs no transport reads")
    }
}

fn input(
    text: Arc<str>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<JsonFieldProjection, JsonReadError> {
    let retained = memory.clone();
    let original = original.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&retained, &original, work.cancellation);
        observe_json_text(
            text.as_bytes(),
            &control,
            &mut work,
            protocol(),
            Some("format"),
        )
        .await
        .map(JsonValueObservation::field)
    })
    .unwrap()
}

fn finish(
    input: &mut ImageDecoder<JsonFieldProjection, JsonReadError>,
    steps: usize,
) -> Result<(JsonFieldProjection, usize), JsonReadError> {
    for polls in 1..1_000_000 {
        if let Some(result) = input.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })? {
            return Ok((result, polls));
        }
    }
    panic!("generic JSON input did not progress")
}

#[test]
fn generic_json_observation_resumes_long_strings_and_nested_raw_values_without_a_tree() {
    let protocol = protocol();
    let mut text = format!(
        r#"{{"ignored":{},"format":7,"format":9}}"#,
        serde_json::to_string(&"한\n\0".repeat(4096)).unwrap()
    );
    for _ in 0..3 {
        text = format!(
            r"{{{:?}:{}}}",
            protocol.raw,
            serde_json::to_string(&text).unwrap()
        );
    }
    let expected = oracle(&text).unwrap().1;
    for steps in [1, 17, 1024] {
        let memory = MemoryBudget::new(4 << 20);
        let cancellation = CancellationToken::new();
        let mut input = input(text.clone().into(), &memory, &cancellation, &cancellation);
        let (actual, polls) = finish(&mut input, steps).unwrap();
        assert_eq!(actual, expected);
        assert!(polls > 1);
        assert_eq!(memory.used(), 0);
        assert!(finish(&mut input, steps).is_err());
    }
}

#[test]
fn generic_json_observation_retires_failed_cancelled_and_abandoned_expansions() {
    let text: Arc<str> = format!(r#"{{"ignored":{:?},"format":1}}"#, "x".repeat(65536)).into();
    for mode in ["original", "invoking", "memory", "zero", "drop", "unwind"] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut input = input(text.clone(), &memory, &original, &invoking);
        assert!(input
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
            .unwrap()
            .is_none());
        let occupied = (mode == "memory").then(|| memory.reserve(memory.available()).unwrap());
        if mode == "original" {
            original.cancel();
        }
        if mode == "invoking" {
            invoking.cancel();
        }
        if mode == "unwind" {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _input = input;
                    panic!("abandoned JSON observation");
                }))
                .is_err()
            );
        } else {
            if mode != "drop" {
                assert!(
                    finish(&mut input, usize::from(mode != "zero")).is_err(),
                    "{mode}"
                );
                assert!(finish(&mut input, 1).is_err());
            }
            drop(input);
        }
        drop(occupied);
        assert_eq!(memory.used(), 0);
    }
}
