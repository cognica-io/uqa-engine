//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn project(text: Arc<str>, steps: usize) -> StorageBackendResult<(FieldProjection, usize)> {
    let memory = MemoryBudget::new(16 << 10);
    let budget = memory.clone();
    let token = CancellationToken::new();
    let original = token.clone();
    let mut input = ImageDecoder::new(&memory, &token, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&budget, &original, work.cancellation);
        field_from(&text, "format", &control, &mut work).await
    })
    .unwrap();
    let result = (1..1_000_000)
        .find_map(|polls| {
            match input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps }) {
                Ok(None) => None,
                Ok(Some(value)) => Some(Ok((value, polls))),
                Err(error) => Some(Err(error)),
            }
        })
        .expect("field input progressed");
    assert_eq!(memory.used(), 0);
    result
}

fn compare_projection(text: &str) {
    let expected = serde_json::from_str::<serde_json::Value>(text).map(|value| FieldProjection {
        present: value.get("format").is_some(),
        foreign: value
            .as_object()
            .is_some_and(|object| object.keys().any(|key| key != "format")),
        unsigned: value.get("format").and_then(serde_json::Value::as_u64),
    });
    let actual = project(text.into(), 1).map(|(value, _)| value);
    match expected {
        Ok(expected) => assert_eq!(actual.unwrap(), expected, "{text}"),
        Err(expected) => {
            let Err(StorageBackendError::Json(actual)) = actual else {
                panic!("{text}: {actual:?}");
            };
            assert_eq!(actual.to_string(), expected.to_string(), "{text}");
            assert_eq!(actual.is_data(), expected.is_data(), "{text}");
            assert_eq!(actual.is_syntax(), expected.is_syntax(), "{text}");
            assert_eq!(actual.is_eof(), expected.is_eof(), "{text}");
        }
    }
}

#[test]
fn marker_field_projection_preserves_public_value_roots_and_overwritten_fields() {
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let keys = protocol::keys(&ProductionControl::new(&memory, &token, &token)).unwrap();
    let mut roots: Vec<String> = [
        "null",
        "true",
        "0",
        "-0",
        "1e999999",
        "[]",
        "{}",
        r#"{"format":3}"#,
        r#"{"format":3,"format":2}"#,
        r#"{"format":null,"\u0066ormat":3}"#,
        r#"{"format":3,"other":0,"other":1}"#,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for value in [
        "null",
        "[]",
        "{}",
        "-0",
        "3",
        "3.0",
        "3e0",
        "18446744073709551616",
        r#""\ud800""#,
    ] {
        roots.push(format!(r#"{{"format":{value}}}"#));
        roots.push(format!(r#"{{"format":{value},"format":3}}"#));
        roots.push(format!(r#"{{"format":3,"other":[{value}]}}"#));
    }
    for root in roots {
        compare_projection(&root);
        for key in [keys.number, keys.raw] {
            let wrapped = format!(r#"{{"{key}":{}}}"#, serde_json::to_string(&root).unwrap());
            compare_projection(&wrapped);
            compare_projection(&format!(r#"{{"format":{wrapped}}}"#));
            compare_projection(&format!(
                r#"{{"{key}":{},"extra":null}}"#,
                serde_json::to_string(&root).unwrap()
            ));
        }
    }
}

#[test]
fn marker_field_projection_validates_discarded_depth_truncation_and_work_windows() {
    let text = r#"{"other":{"format":2,"nested":[null,"한🙂"]},"format":3}"#;
    for end in 0..=text.len() {
        if text.is_char_boundary(end) {
            compare_projection(&text[..end]);
        }
    }
    for depth in [125, 126, 127, 128] {
        compare_projection(&format!(
            r#"{{"other":{}null{},"format":3}}"#,
            "[".repeat(depth),
            "]".repeat(depth)
        ));
    }
    let text: Arc<str> = format!(
        r#"{{"other":{{"first":0,"{}": [{}null]}},"format":3}}"#,
        "한🙂".repeat(16384),
        "null,".repeat(32768)
    )
    .into();
    let mut visits = Vec::new();
    for steps in [1, 7, 64] {
        let (projection, polls) = project(Arc::clone(&text), steps).unwrap();
        assert_eq!(
            projection,
            FieldProjection {
                present: true,
                foreign: true,
                unsigned: Some(3)
            }
        );
        visits.push(polls);
    }
    assert!(visits[0] > visits[1] && visits[1] > visits[2]);
}
