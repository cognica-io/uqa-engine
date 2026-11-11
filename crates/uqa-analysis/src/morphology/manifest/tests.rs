//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{canonical_with, equal_with, inventory_with, pinned_digest_with, scripts_with};
use crate::morphology::DictionaryError;
use serde_json::{json, Value};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::MemoryBudget,
    CancellationToken,
};

const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

mod input;
mod provenance;
pub(crate) use provenance::{verify_fields, verify_retirement_and_errors, Fields};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("native provenance validation must only use its original values")
    }
}

fn complete<T>(input: &mut ImageDecoder<T, DictionaryError>, steps: usize) -> (T, usize) {
    let mut calls = 0;
    loop {
        calls += 1;
        assert!(calls < 100_000);
        if let Some(value) = input
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })
            .unwrap()
        {
            return (value, calls);
        }
    }
}

fn canonical_fixture() -> (Value, Vec<u8>) {
    let text = format!(
        "{}{}",
        "x".repeat(4095),
        "\0\n\t\r\u{0008}\u{000c}\"\\/中𠮷\u{2028}\u{2029}".repeat(3000)
    );
    let raw_number = format!("1{}e+12345", "7".repeat(20_000));
    let number: Value = serde_json::from_str(&raw_number).unwrap();
    let mut deep = json!("leaf");
    for _ in 0..120 {
        deep = Value::Array(vec![deep]);
    }
    let wide: serde_json::Map<_, _> = (0..5000)
        .rev()
        .map(|index| (format!("k{index:04}"), json!(index)))
        .collect();
    let mut root = serde_json::Map::new();
    root.insert("z".to_owned(), Value::String(text.clone()));
    root.insert("c".to_owned(), Value::Object(wide));
    root.insert("b".to_owned(), deep);
    root.insert("a".to_owned(), number.clone());
    let expected_wide = (0..5000)
        .map(|i| format!("\"k{i:04}\":{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let expected = format!(
        "{{\"a\":{},\"b\":{}\"leaf\"{},\"c\":{{{expected_wide}}},\"z\":{}}}",
        serde_json::to_string(&number).unwrap(),
        "[".repeat(120),
        "]".repeat(120),
        serde_json::to_string(&text).unwrap()
    );
    (Value::Object(root), expected.into_bytes())
}

#[test]
fn native_provenance_canonical_output_preserves_unknown_values_escaping_order_and_precision() {
    let (value, expected) = canonical_fixture();
    let value = Arc::new(value);
    for steps in [1, 13, 512] {
        let measured = allocation_counter::measure(|| {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let value = value.clone();
            let mut input = ImageDecoder::new(&memory, &token, |mut channel| async move {
                canonical_with(&value, &mut channel.input()).await
            })
            .unwrap();
            let (actual, calls) = complete(&mut input, steps);
            if steps == 1 {
                assert!(calls > 10_000);
            }
            assert_eq!(actual, expected);
            assert_eq!(memory.used(), 0);
            assert!(input.advance(&mut NoRead, WINDOW).is_err());
        });
        assert_eq!(measured.bytes_current, 0);
    }
}

#[test]
fn native_provenance_canonical_interruption_retires_output_and_ordering_workspace() {
    let value = Arc::new(canonical_fixture().0);
    for stop in [0, 1, 2, 64, 512, 4096, 10_000] {
        for mode in ["drop", "cancel", "window"] {
            let measured = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let value = value.clone();
                let mut input = ImageDecoder::new(&memory, &token, |mut channel| async move {
                    canonical_with(&value, &mut channel.input()).await
                })
                .unwrap();
                for _ in 0..stop {
                    assert!(input.advance(&mut NoRead, WINDOW).unwrap().is_none());
                }
                match mode {
                    "cancel" => {
                        token.cancel();
                        assert!(matches!(
                            input.advance(&mut NoRead, WINDOW),
                            Err(DictionaryError::Input(ImageIoError::Cancelled(_)))
                        ));
                    }
                    "window" => {
                        assert!(matches!(
                            input.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 0 }),
                            Err(DictionaryError::Input(ImageIoError::Invalid))
                        ));
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
}

#[test]
fn native_provenance_runtime_equality_keeps_all_native_json_distinctions() {
    let (base, _) = canonical_fixture();
    let mut different_text = base.clone();
    let mut last = different_text["z"].as_str().unwrap().to_owned();
    assert_eq!(last.pop(), Some('\u{2029}'));
    last.push('\u{2028}');
    different_text["z"] = Value::String(last);
    let mut different_key = base.clone();
    let object = different_key["c"].as_object_mut().unwrap();
    let prior = object.remove("k4999").unwrap();
    object.insert("other".to_owned(), prior);
    let mut different_value = base.clone();
    different_value["c"]["k4999"] = json!(false);
    let pairs = vec![
        (base.clone(), base.clone()),
        (base.clone(), different_text),
        (base.clone(), different_key),
        (base, different_value),
        (json!(1), json!(1.0)),
        (json!(-0.0), json!(0.0)),
        (json!([null, true]), json!([null, true])),
        (json!([null]), json!([])),
        (json!(false), json!(true)),
    ];
    for (left, right) in pairs {
        let expected = left == right;
        let pair = Arc::new((left, right));
        let measured = allocation_counter::measure(|| {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let pair = pair.clone();
            let mut input = ImageDecoder::new(&memory, &token, |mut channel| async move {
                equal_with(&pair.0, &pair.1, &mut channel.input()).await
            })
            .unwrap();
            let (actual, _) = complete(&mut input, 1);
            assert_eq!(actual, expected);
            assert_eq!(memory.used(), 0);
        });
        assert_eq!(measured.bytes_current, 0);
    }
}

fn script_names() -> Vec<String> {
    let mut names: Vec<String> = ["COMMON", "INHERITED", "UNKNOWN"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    for index in 0..257 {
        names.push(format!(
            "{}{}{}",
            "A".repeat(6000),
            char::from(b'A' + (index / 26) as u8),
            char::from(b'A' + (index % 26) as u8)
        ));
    }
    names
}

#[test]
fn native_provenance_vocabularies_and_inventories_resume_complete_native_validation() {
    for invalid in [false, true] {
        let measured = allocation_counter::measure(|| {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut names = script_names();
            if invalid {
                names.last_mut().unwrap().push('a');
            }
            let value = serde_json::to_value(&names).unwrap();
            let mut input = ImageDecoder::new(&memory, &token, |mut channel| async move {
                scripts_with(&value, &mut channel.input()).await
            })
            .unwrap();
            if invalid {
                let error = loop {
                    match input.advance(&mut NoRead, WINDOW) {
                        Ok(None) => (),
                        Ok(Some(_)) => panic!("invalid script vocabulary was published"),
                        Err(error) => break error,
                    }
                };
                assert!(error
                    .to_string()
                    .contains("invalid Unicode script vocabulary"));
            } else {
                let (actual, calls) = complete(&mut input, 1);
                assert_eq!(actual, names);
                assert!(calls > 1000);
                for value in actual {
                    assert_eq!(value.capacity(), value.len());
                }
            }
            drop(input);
            assert_eq!(memory.used(), 0);
        });
        assert_eq!(measured.bytes_current, 0);
    }
    let measured = allocation_counter::measure(|| {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let names: Vec<_> = (0..5000).map(|index| format!("item{index:04}")).collect();
        let value = Value::Array(
            names
                .iter()
                .rev()
                .map(|name| json!({"path": name, "bytes": 1, "sha256": "0".repeat(64)}))
                .collect(),
        );
        let mut input = ImageDecoder::new(&memory, &token, |mut channel| async move {
            let expected: Vec<_> = names.iter().map(String::as_str).collect();
            inventory_with(&value, "path", &expected, &mut channel.input()).await
        })
        .unwrap();
        let ((), calls) = complete(&mut input, 1);
        assert!(calls > 5000);
        assert_eq!(memory.used(), 0);
    });
    assert_eq!(measured.bytes_current, 0);
}

#[test]
fn native_provenance_digest_search_preserves_the_first_marker_and_exact_errors() {
    for prefix in [1, 4093, 4095, 4096, 8191, 160_000] {
        for suffix in [
            "0".repeat(64),
            String::new(),
            "bad".to_owned(),
            format!("{}@sha256:{}", "0".repeat(64), "0".repeat(64)),
        ] {
            let measured = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let value = Value::String(format!("{}@sha256:{suffix}", "x".repeat(prefix)));
                let valid = suffix.len() == 64;
                let mut input = ImageDecoder::new(&memory, &token, |mut channel| async move {
                    pinned_digest_with(&value, &mut channel.input()).await
                })
                .unwrap();
                let mut calls = 0;
                let result = loop {
                    calls += 1;
                    match input.advance(&mut NoRead, WINDOW) {
                        Ok(None) => (),
                        result => break result,
                    }
                };
                assert!(calls > prefix / 4096);
                assert_eq!(result.is_ok(), valid);
                if let Err(error) = result {
                    assert!(error.to_string().contains(if suffix.is_empty() {
                        "missing nonempty identity string"
                    } else {
                        "invalid hexadecimal digest or commit"
                    }));
                }
                assert_eq!(memory.used(), 0);
            });
            assert_eq!(measured.bytes_current, 0);
        }
    }
    for text in ["@sha256:bad@sha256:again", "no digest", ""] {
        let result = crate::morphology::work::inline(pinned_digest_with(&json!(text), &mut ()));
        assert!(result
            .unwrap_err()
            .to_string()
            .contains(if text.is_empty() {
                "missing nonempty identity string"
            } else {
                "Docker image is not digest pinned"
            }));
    }
}
