//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

const FORMAT: TypeRecordFormat = TypeRecordFormat {
    label: "test",
    definition_context: "test definition",
    oid_context: "test OID",
    format_key: "test_format",
    format_field: "format",
    version: 1,
    prefix: "test:",
    oid_prefix: "test_oid:",
};

fn oracle(text: &str) -> StorageBackendResult<()> {
    let marker: serde_json::Map<String, serde_json::Value> = serde_json::from_str(text)?;
    let version = match (marker.len(), marker.get(FORMAT.format_field)) {
        (1, Some(serde_json::Value::Number(version))) => version.as_u64(),
        _ => None,
    }
    .ok_or_else(|| TypeRecordFormat::corrupt("malformed test catalog format marker"))?;
    if version != 1 {
        return Err(TypeRecordFormat::corrupt(format!(
            "unsupported test catalog format {version}"
        )));
    }
    Ok(())
}

fn observation(result: &StorageBackendResult<()>) -> String {
    match result {
        Ok(()) => "valid".into(),
        Err(StorageBackendError::Serde(_)) => "JSON error".into(),
        Err(error) => error.to_string(),
    }
}

fn input(
    text: Arc<str>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<(), StorageBackendError> {
    let budget = memory.clone();
    let original = original.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&budget, &original, work.cancellation);
        check_from(&FORMAT, &text, &control, &mut work).await
    })
    .unwrap()
}

fn finish(
    input: &mut ImageDecoder<(), StorageBackendError>,
    steps: usize,
) -> StorageBackendResult<usize> {
    for polls in 1..1_000_000 {
        if input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
            .is_some()
        {
            return Ok(polls);
        }
    }
    panic!("marker input did not progress")
}

fn compare(text: &str) {
    let expected = observation(&oracle(text));
    assert_eq!(
        observation(&check(&FORMAT, text)),
        expected,
        "inline: {text}"
    );
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let mut input = input(text.into(), &memory, &token, &token);
    let actual = finish(&mut input, 1).map(|_| ());
    assert_eq!(observation(&actual), expected, "retained: {text}");
    assert_eq!(memory.used(), 0);
    assert!(finish(&mut input, 1).is_err());
}

#[test]
fn retained_type_markers_preserve_generic_scalar_duplicate_and_overwritten_value_semantics() {
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let keys = protocol::keys(&ProductionControl::new(&memory, &token, &token)).unwrap();
    let mut values: Vec<String> = [
        "null",
        "true",
        "[]",
        "{}",
        "\"1\"",
        "-0",
        "0",
        "-1",
        "1",
        "2",
        "1.0",
        "1e0",
        "1e999999",
        "18446744073709551615",
        "18446744073709551616",
        "01",
        "1e",
        "NaN",
        "\"\\ud800\"",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for key in [keys.number, keys.raw] {
        for value in [
            "1",
            "-0",
            "1e0",
            "1.0",
            "18446744073709551616",
            "1e999999",
            "",
            "bad",
            " 1",
            "1 ",
            "1x",
            "null",
            "[]",
            "{}",
        ] {
            let encoded = serde_json::to_string(value).unwrap();
            values.push(format!(r#"{{"{key}":{encoded}}}"#));
            values.push(format!(r#"{{"{key}":{encoded},"extra":null}}"#));
            values.push(format!(r#"{{"extra":null,"{key}":{encoded}}}"#));
        }
        for value in ["1", "null", "[]", "{}"] {
            values.push(format!(r#"{{"{key}":{value}}}"#));
        }
    }
    let mut nested = "1".to_owned();
    for _ in 0..8 {
        nested = format!(
            r#"{{"{}":{}}}"#,
            keys.raw,
            serde_json::to_string(&nested).unwrap()
        );
        values.push(nested.clone());
    }
    for value in values {
        for text in [
            format!(r#"{{"format":{value}}}"#),
            format!(r#"{{"format":{value},"format":1}}"#),
            format!(r#"{{"format":1,"\u0066ormat":{value}}}"#),
            format!(r#"{{"format":1,"other":[{value}]}}"#),
            format!(r#"{{"format":{{"ordinary":{value}}},"format":1}}"#),
        ] {
            compare(&text);
        }
    }
}

#[test]
fn retained_type_markers_validate_truncation_depth_and_all_discarded_tokens() {
    let text = r#"{"format":{"ordinary":[null,"한🙂"]},"format":1}"#;
    for end in 0..=text.len() {
        if text.is_char_boundary(end) {
            compare(&text[..end]);
        }
    }
    for text in [
        "[]",
        "null",
        "1",
        "{}",
        "{\"format\":1} false",
        r#"{"format":1,"other":0,"other":0}"#,
        r#"{"format":{"k":"\udc00"},"format":1}"#,
        r#"{"format":[1,],"format":1}"#,
        r#"{"format":1,"\ud800":null}"#,
    ] {
        compare(text);
    }
    for depth in [15, 16, 125, 126, 127, 128, 129] {
        compare(&format!(
            r#"{{"format":{}null{},"format":1}}"#,
            "[".repeat(depth),
            "]".repeat(depth)
        ));
    }
}

#[test]
fn retained_type_markers_bound_discarded_value_memory_and_resume_original_work_windows() {
    let text: Arc<str> = format!(
        r#"{{"format":[{}"{}"],"format":1}}"#,
        "null,".repeat(32768),
        "한🙂".repeat(16384)
    )
    .into();
    assert!(oracle(&text).is_ok());
    let mut visits = Vec::new();
    for steps in [1, 7, 64] {
        let memory = MemoryBudget::new(16 << 10);
        let token = CancellationToken::new();
        let mut input = input(Arc::clone(&text), &memory, &token, &token);
        let allocations = allocation_counter::measure(|| {
            visits.push(finish(&mut input, steps).unwrap());
        });
        assert!(allocations.bytes_max < 16 << 10, "{allocations:?}");
        assert_eq!(memory.used(), 0);
    }
    assert!(visits[0] > visits[1] && visits[1] > visits[2]);
    assert!(visits[0] > text.len() / 4096);
}

#[test]
fn retained_type_markers_retire_original_and_invoking_cancellation_and_partial_values() {
    let text: Arc<str> = format!(
        r#"{{{},"format":1}}"#,
        (0..64)
            .map(|i| format!(r#""key_{i}_{}":{{"nested":[1,2,3]}}"#, "한🙂".repeat(256)))
            .collect::<Vec<_>>()
            .join(",")
    )
    .into();
    for cut in [1, 16, 128, 512] {
        for terminal in ["original", "invoking", "memory", "zero", "drop", "unwind"] {
            let memory = MemoryBudget::new(1 << 20);
            let sibling = memory.reserve(17).unwrap();
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = input(Arc::clone(&text), &memory, &original, &invoking);
            for _ in 0..cut {
                assert!(input
                    .advance(
                        &mut std::io::empty(),
                        ImageDecodeWindow { bytes: 1, steps: 1 }
                    )
                    .unwrap()
                    .is_none());
            }
            let occupied =
                (terminal == "memory").then(|| memory.reserve(memory.available()).unwrap());
            match terminal {
                "original" => original.cancel(),
                "invoking" => invoking.cancel(),
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _owner = input;
                        panic!("retire marker input");
                    }));
                    assert_eq!(memory.used(), sibling.bytes());
                    continue;
                }
                _ => (),
            }
            if terminal != "drop" {
                let error = finish(&mut input, usize::from(terminal != "zero")).unwrap_err();
                match terminal {
                    "original" | "invoking" => {
                        assert!(matches!(error, StorageBackendError::Cancelled(_)));
                    }
                    "memory" => assert!(matches!(error, StorageBackendError::Memory(_))),
                    "zero" => (),
                    _ => unreachable!(),
                }
                assert!(finish(&mut input, 1).is_err());
            }
            drop((input, occupied));
            assert_eq!(memory.used(), sibling.bytes(), "{terminal}/{cut}");
        }
    }
}
