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
        (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{text}");
            assert_eq!(actual.is_data(), expected.is_data(), "{text}");
            assert_eq!(actual.is_syntax(), expected.is_syntax(), "{text}");
            assert_eq!(actual.is_eof(), expected.is_eof(), "{text}");
        }
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

#[test]
fn generic_json_diagnostics_preserve_scalar_visitor_boundaries_and_nested_locations() {
    let protocol = protocol();
    for key in [protocol.number, protocol.raw] {
        let mut values = [
            "null",
            "true",
            "false",
            "42",
            "-0",
            "-12",
            "1.2",
            "1e9999",
            "[]",
            "{}",
            "[invalid",
            "{invalid",
            "01",
            "-",
            "trux",
            "\"\\ud800\"",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        for text in [
            "",
            " ",
            "\n",
            "\n1",
            "1\n",
            "\t-1",
            "0",
            "-0",
            "01",
            "1 ",
            " 1",
            "1.0",
            "1e99999",
            "-",
            "+1",
            "1e",
            "1e-",
            "1e+",
            "1e!",
            "1.",
            "1.a",
            "- 1",
            "-x1",
            "-\n1",
            "-한",
            "1e!0",
            "1e\n0",
            "1.\n0",
            "-01",
            "[]",
            "{}",
            "no",
            "null",
            "[1,]",
            "[1] extra",
            "[\n1,\n]",
            "[\n\"한🙂\",\nfalse,]",
            "{\n\"한🙂\":invalid}",
        ] {
            values.push(serde_json::to_string(text).unwrap());
            let wrapped = format!(
                r"{{{:?}:{}}}",
                protocol.raw,
                serde_json::to_string(text).unwrap()
            );
            values.push(serde_json::to_string(&wrapped).unwrap());
        }
        for value in values {
            for tail in [
                "}",
                " }",
                "\n}",
                ",\"extra\":invalid}",
                ", ",
                "",
                "]",
                " false}",
            ] {
                let text =
                    format!("{{\n\"before\":\"한🙂\",\n\"selected\":{{{key:?}:\n{value}{tail}}}");
                compare(&text);
            }
        }
    }
}

#[test]
fn generic_json_scalar_type_errors_precede_generic_container_depth() {
    let keys = protocol();
    for depth in [0, 124, 125, 126, 127, 128] {
        for key in [keys.number, keys.raw] {
            for payload in [
                "[]", "{}", "[invalid", "{invalid", "null", "\"1\"", "\"[1]\"",
            ] {
                let text = format!(
                    "{}{{{key:?}:{payload}}}{}",
                    "[".repeat(depth),
                    "]".repeat(depth),
                );
                compare(&text);
                compare(&format!(r#"{{"format":{text}}}"#));
                compare(&format!(
                    r"{{{:?}:{}}}",
                    keys.raw,
                    serde_json::to_string(&text).unwrap()
                ));
            }
        }
    }
}

#[test]
fn generic_json_diagnostics_match_original_truncation_and_syntax_locations() {
    let protocol = protocol();
    for body in [
        r#"{"before":"한🙂","format":[null,true,false,1.2,-0,"escaped\\value"]}"#.to_owned(),
        format!(r#"{{"format":{{{:?}:"1"}},"extra":null}}"#, protocol.number),
        format!(
            r#"{{"format":{{{:?}:"[1,2]"}},"extra":null}}"#,
            protocol.raw
        ),
    ] {
        for end in 0..=body.len() {
            if body.is_char_boundary(end) {
                compare(&body[..end]);
            }
        }
        for tail in [",", " false", " {}", "\n!", "\n\n[\n"] {
            compare(&(body.clone() + tail));
        }
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
) -> ImageDecoder<JsonFieldProjection, JsonInputError> {
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
    input: &mut ImageDecoder<JsonFieldProjection, JsonInputError>,
    steps: usize,
) -> Result<(JsonFieldProjection, usize), JsonInputError> {
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

struct FailingWork {
    remaining: usize,
    visited: usize,
    memory_failure: bool,
}

impl JsonReadWork for FailingWork {
    type Error = JsonReadError;

    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        self.visited += 1;
        if self.remaining != 0 {
            self.remaining -= 1;
            return Ok(());
        }
        if self.memory_failure {
            return Err(MemoryBudget::new(0).reserve(1).err().unwrap().into());
        }
        let token = CancellationToken::new();
        token.cancel();
        token.check().map_err(Into::into)
    }
}

#[test]
fn generic_json_scalar_errors_keep_resources_terminal_at_every_work_boundary() {
    let keys = protocol();
    let mut text = format!(r"{{{:?}:{:?}}}", keys.number, "9".repeat(8192));
    for _ in 0..2 {
        text = format!(
            r"{{{:?}:{}}}",
            keys.raw,
            serde_json::to_string(&text).unwrap()
        );
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut work = FailingWork {
        remaining: usize::MAX,
        visited: 0,
        memory_failure: false,
    };
    super::super::work::inline(observe_json_text(
        text.as_bytes(),
        &control,
        &mut work,
        keys,
        None,
    ))
    .unwrap();
    assert!(work.visited > 20);
    let checkpoints = work.visited;
    for memory_failure in [false, true] {
        for remaining in 0..checkpoints {
            let mut work = FailingWork {
                remaining,
                visited: 0,
                memory_failure,
            };
            let error = super::super::work::inline(observe_json_text(
                text.as_bytes(),
                &control,
                &mut work,
                keys,
                None,
            ))
            .err()
            .expect("the selected work boundary fails");
            assert_eq!(
                matches!(&error, JsonInputError::Memory(_)),
                memory_failure,
                "{remaining}: {error:?}"
            );
            assert_eq!(
                matches!(&error, JsonInputError::Cancelled(_)),
                !memory_failure,
                "{remaining}: {error:?}"
            );
            assert_eq!(work.visited, remaining + 1);
            drop(error);
            assert_eq!(memory.used(), 0);
        }
    }
}
