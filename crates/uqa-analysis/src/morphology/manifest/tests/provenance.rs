//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    canonical_fixture, script_names, Arc, CancellationToken, ImageDecodeWindow, ImageDecoder,
    ImageIoError, MemoryBudget, NoRead, Value, WINDOW,
};

pub(crate) type Fields = (Value, Vec<String>);
type Input<E> = fn(Arc<Vec<u8>>, &MemoryBudget, &CancellationToken) -> ImageDecoder<Fields, E>;

fn expanded(mut json: Value) -> Value {
    json["extra"] = canonical_fixture().0;
    let runtime = &mut json["reference"]["runtime"];
    runtime["extension"] = serde_json::json!(["long".repeat(40_000), 1.0, -0.0, null, true]);
    json["model"]["runtime"] = runtime.clone();
    json["model"]["unicode_scripts"]
        .as_array_mut()
        .unwrap()
        .extend(script_names().into_iter().skip(3).map(Value::String));
    json["reference"]["docker_image"] =
        Value::String(format!("{}@sha256:{}", "x".repeat(160_000), "0".repeat(64)));
    json
}

fn wire(value: &Value) -> Arc<Vec<u8>> {
    Arc::new(crate::morphology::manifest::canonical(value).unwrap())
}

pub(crate) fn verify_fields<E>(fixture: Value, input: Input<E>)
where
    E: From<ImageIoError> + std::fmt::Debug,
{
    let expected = expanded(fixture);
    let bytes = wire(&expected);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut first = input(bytes.clone(), &memory, &token);
    let measured = allocation_counter::measure(|| {
        assert!(first.advance(&mut NoRead, WINDOW).unwrap().is_none());
    });
    assert!(
        measured.bytes_total < 64 << 10,
        "the first syntax window materialized {} bytes before yielding",
        measured.bytes_total
    );
    drop(first);
    assert_eq!(memory.used(), 0);
    for steps in [1, 13, 512] {
        let measured = allocation_counter::measure(|| {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = input(bytes.clone(), &memory, &token);
            let mut calls = 0;
            let (json, scripts) = loop {
                calls += 1;
                assert!(calls < 100_000);
                if let Some(value) = input
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })
                    .unwrap()
                {
                    break value;
                }
            };
            if steps == 1 {
                assert!(calls > 10_000);
            }
            assert_eq!(json, expected);
            assert_eq!(
                serde_json::to_value(&scripts).unwrap(),
                expected["model"]["unicode_scripts"]
            );
            assert_eq!(wire(&json).as_ref(), bytes.as_ref());
            for script in scripts {
                assert_eq!(script.capacity(), script.len());
            }
            assert_eq!(memory.used(), 0);
            assert!(input.advance(&mut NoRead, WINDOW).is_err());
        });
        assert_eq!(measured.bytes_current, 0);
    }
}

pub(crate) fn verify_retirement_and_errors<E>(
    fixture: Value,
    input: Input<E>,
    ordinary: fn(&[u8]) -> Result<Fields, E>,
) where
    E: From<ImageIoError> + std::fmt::Debug + std::fmt::Display,
{
    let expected = expanded(fixture);
    let bytes = wire(&expected);
    let calls = {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = input(bytes.clone(), &memory, &token);
        let mut calls = 0;
        loop {
            calls += 1;
            assert!(calls < 100_000);
            if input.advance(&mut NoRead, WINDOW).unwrap().is_some() {
                break;
            }
        }
        assert_eq!(memory.used(), 0);
        calls
    };
    let mut stops = vec![0, 1, 2, 64, 512, 4096, 10_000, calls / 2, calls - 1];
    stops.sort_unstable();
    stops.dedup();
    for stop in stops {
        for mode in ["drop", "cancel", "window"] {
            let measured = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let mut input = input(bytes.clone(), &memory, &token);
                for _ in 0..stop {
                    assert!(input.advance(&mut NoRead, WINDOW).unwrap().is_none());
                }
                match mode {
                    "cancel" => {
                        token.cancel();
                        assert!(input.advance(&mut NoRead, WINDOW).is_err());
                    }
                    "window" => {
                        assert!(input
                            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 0 })
                            .is_err());
                    }
                    _ => (),
                }
                if mode != "drop" {
                    assert!(input.advance(&mut NoRead, WINDOW).is_err());
                }
                drop(input);
                assert_eq!(memory.used(), 0);
            });
            assert_eq!(measured.bytes_current, 0, "{stop} {mode}");
        }
    }
    verify_errors(&expected, input, ordinary);
}

fn verify_errors<E>(expected: &Value, input: Input<E>, ordinary: fn(&[u8]) -> Result<Fields, E>)
where
    E: From<ImageIoError> + std::fmt::Debug + std::fmt::Display,
{
    for (case, reason) in [
        (0, "manifest is not canonical JSON"),
        (1, "unsupported neutral model format"),
        (2, "invalid Unicode script vocabulary"),
        (3, "duplicate identity or invalid byte count"),
        (4, "reference and model runtimes differ"),
        (5, "EOF while parsing an object at line 1 column 1"),
    ] {
        let mut value = expected.clone();
        match case {
            0 | 1 => {
                value["format"] = serde_json::json!(false);
                value["model"]["unicode_scripts"] = serde_json::json!(["invalid"]);
            }
            2 => {
                value["model"]["unicode_scripts"] =
                    serde_json::json!(["COMMON", "INHERITED", "UNKNOWN", "bad"]);
                value["files"][0]["bytes"] = serde_json::json!(0);
            }
            3 => {
                value["files"][0]["bytes"] = serde_json::json!(0);
                value["model"]["runtime"]["extension"] = serde_json::json!(false);
            }
            4 => value["model"]["runtime"]["extension"] = serde_json::json!(false),
            5 => (),
            _ => unreachable!(),
        }
        let mut bytes = crate::morphology::manifest::canonical(&value).unwrap();
        if case == 0 {
            bytes.insert(0, b' ');
        }
        if case == 5 {
            bytes = b"{".to_vec();
        }
        let bytes = Arc::new(bytes);
        let measured = allocation_counter::measure(|| {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = input(bytes.clone(), &memory, &token);
            let error = loop {
                match input.advance(&mut NoRead, WINDOW) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("invalid provenance was published"),
                    Err(error) => break error,
                }
            };
            assert!(error.to_string().ends_with(reason), "case {case}: {error}");
            assert_eq!(error.to_string(), ordinary(&bytes).unwrap_err().to_string());
            assert!(input.advance(&mut NoRead, WINDOW).is_err());
            drop(input);
            assert_eq!(memory.used(), 0);
        });
        assert_eq!(measured.bytes_current, 0, "case {case}");
    }
}
