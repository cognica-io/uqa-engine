//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::image_io::{ImageDecodeWindow, ImageDecoder};
use std::sync::Arc;

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("object projection must borrow its original source")
    }
}

fn input(
    bytes: Arc<[u8]>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    expected: Option<serde_json::Value>,
) -> ImageDecoder<usize, JsonReadError> {
    let retained = memory.clone();
    let original = original.clone();
    ImageDecoder::new(memory, invoking, |mut channel| async move {
        let actual =
            JsonObjectProjection::read_from(&bytes, &retained, &original, &mut channel.input())
                .await?;
        if let Some(expected) = expected {
            let expected = expected.as_object().expect("object oracle");
            assert_eq!(actual.len(), expected.len());
            assert_eq!(actual.is_empty(), expected.is_empty());
            assert_eq!(
                actual.iter().map(|(key, _)| key).collect::<Vec<_>>(),
                expected.keys().map(String::as_str).collect::<Vec<_>>()
            );
            for (name, value) in actual.iter() {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(value).unwrap(),
                    expected[name]
                );
                let offset = value.as_ptr() as usize - bytes.as_ptr() as usize;
                assert_eq!(value, &bytes[offset..offset + value.len()]);
                assert_eq!(actual.get(name).unwrap().as_ptr(), value.as_ptr());
            }
        }
        Ok(actual.len())
    })
    .unwrap()
}

fn finish(
    input: &mut ImageDecoder<usize, JsonReadError>,
    steps: usize,
) -> Result<(usize, usize), JsonReadError> {
    for polls in 1..1_000_000 {
        if let Some(count) = input.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })? {
            return Ok((count, polls));
        }
    }
    panic!("object projection stopped progressing")
}

#[test]
fn retained_object_projection_preserves_duplicate_fields_borrowed_ranges_and_work_windows() {
    let text = format!(
        r#"{{"huge":1e99999,"zero":-0,"z":["{}",{{"한🙂":true}}],"key{}":null,"z":{{"last":1.00}},"\u007a":false}}"#,
        "x".repeat(65536),
        "한🙂".repeat(4096)
    );
    let bytes: Arc<[u8]> = text.as_bytes().into();
    let expected: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut polls = Vec::new();
    for steps in [1, 7, 64] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut input = input(
            Arc::clone(&bytes),
            &memory,
            &original,
            &invoking,
            Some(expected.clone()),
        );
        let (count, visits) = finish(&mut input, steps).unwrap();
        assert_eq!(count, 4);
        polls.push(visits);
        assert_eq!(memory.used(), 0);
        assert!(finish(&mut input, steps).is_err());
    }
    assert!(polls[0] > polls[1] && polls[1] > polls[2]);
    assert!(polls[0] > bytes.len() / 4096);
}

#[test]
fn retained_object_projection_validates_overwritten_values_depth_and_every_truncated_prefix() {
    let text = r#"{"a":{"b":[null,"한🙂"]},"z":false}"#;
    let mut cases: Vec<Vec<u8>> = (0..text.len())
        .map(|end| text.as_bytes()[..end].to_vec())
        .collect();
    cases.extend([
        br#"{"a":"\ud800","a":null}"#.to_vec(),
        br#"{"a":{"x":"\udc00"},"a":null}"#.to_vec(),
        br#"{"a":00,"a":null}"#.to_vec(),
        br#"{"a":[1,]}"#.to_vec(),
        br#"{"a":null} false"#.to_vec(),
        br#"{"a":null,"\ud800":1}"#.to_vec(),
        b"{\"a\":\"\xff\"}".to_vec(),
        b"[]".to_vec(),
        b"null".to_vec(),
        text.as_bytes().to_vec(),
    ]);
    for depth in [15, 16, 125, 126, 127, 128, 129] {
        cases.push(
            format!(r#"{{"a":{}null{}}}"#, "[".repeat(depth), "]".repeat(depth)).into_bytes(),
        );
    }
    for bytes in cases {
        let expected = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .filter(serde_json::Value::is_object);
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = input(
            bytes.clone().into(),
            &memory,
            &token,
            &token,
            expected.clone(),
        );
        let actual = finish(&mut input, 1);
        assert_eq!(actual.is_ok(), expected.is_some(), "{bytes:?}");
        if let Err(error) = actual {
            assert!(matches!(error, JsonReadError::InvalidJson));
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn retained_object_projection_retires_admitted_prefixes_on_each_terminal_boundary() {
    let text = serde_json::to_vec(
        &(0..64)
            .map(|i| (format!("key_{i:03}_{}", "한🙂".repeat(256)), vec![i; 4]))
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .unwrap();
    let bytes: Arc<[u8]> = text.into();
    for cut in [1, 16, 128, 512] {
        for terminal in ["original", "invoking", "memory", "zero", "drop", "unwind"] {
            let memory = MemoryBudget::new(1 << 20);
            let sibling = memory.reserve(17).unwrap();
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = input(Arc::clone(&bytes), &memory, &original, &invoking, None);
            for _ in 0..cut {
                assert!(input
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                    .unwrap()
                    .is_none());
            }
            let occupied = if terminal == "memory" {
                Some(memory.reserve(memory.available()).unwrap())
            } else {
                None
            };
            match terminal {
                "original" => original.cancel(),
                "invoking" => invoking.cancel(),
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _owner = input;
                        panic!("retire object projection");
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
                        assert!(matches!(error, JsonReadError::Cancelled(_)));
                    }
                    "memory" => assert!(matches!(error, JsonReadError::Memory(_))),
                    "zero" => assert!(matches!(error, JsonReadError::InvalidJson)),
                    _ => unreachable!(),
                }
                assert!(finish(&mut input, 1).is_err());
            }
            drop((input, occupied));
            assert_eq!(memory.used(), sibling.bytes(), "{terminal}/{cut}");
        }
    }
}
