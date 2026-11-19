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
        panic!("JSON source is already retained")
    }
}

fn input(
    bytes: Arc<[u8]>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<JsonDocument, JsonInputError> {
    let retained = memory.clone();
    let original = original.clone();
    ImageDecoder::new(memory, invoking, |mut channel| async move {
        JsonDocument::read_from(
            &bytes,
            &retained,
            &original,
            &mut channel.input(),
            protocol(),
        )
        .await
    })
    .unwrap()
}

fn finish(
    input: &mut ImageDecoder<JsonDocument, JsonInputError>,
    steps: usize,
) -> Result<(JsonDocument, usize), JsonInputError> {
    for polls in 1..1_000_000 {
        if let Some(value) = input.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })? {
            return Ok((value, polls));
        }
    }
    panic!("JSON document input stopped progressing")
}

#[test]
fn native_json_document_retires_overwritten_nodes_and_accounts_for_actual_allocations() {
    let text = format!(
        r#"{{"x":[{}],"x":null}}"#,
        std::iter::repeat_n("\"value\"", 4096)
            .collect::<Vec<_>>()
            .join(",")
    );
    let retained = |bytes: &[u8]| {
        let memory = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut output = None;
        let measured = allocation_counter::measure(|| {
            output = Some(JsonDocument::read_with_control(bytes, &control, protocol()).unwrap());
        });
        assert_eq!(
            usize::try_from(measured.bytes_current).unwrap(),
            memory.used()
        );
        assert!(usize::try_from(measured.bytes_max).unwrap() <= memory.peak());
        let used = memory.used();
        drop(output);
        assert_eq!(memory.used(), 0);
        used
    };
    assert_eq!(retained(text.as_bytes()), retained(br#"{"x":null}"#));
}

#[test]
fn native_json_document_resumes_native_input_and_output_under_original_work_windows() {
    let text = format!(
        r#"{{"key{}":{{"s":"{}"}},"z":1e99,"a":[true,null,3]}}"#,
        "k".repeat(16384),
        "한🙂".repeat(8192)
    );
    let expected = serde_json::from_str::<serde_json::Value>(&text)
        .unwrap()
        .to_string();
    let bytes: Arc<[u8]> = text.into_bytes().into();
    let mut observations = Vec::new();
    for steps in [1, 7, 64] {
        let memory = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let mut input = input(Arc::clone(&bytes), &memory, &token, &token);
        let (document, polls) = finish(&mut input, steps).unwrap();
        observations.push(polls);
        let retained = memory.clone();
        let original = token.clone();
        let mut output = ImageDecoder::<Budgeted<String>, JsonInputError>::new(
            &memory,
            &token,
            |mut channel| async move {
                let mut work = channel.input();
                let control = ProductionControl::new(&retained, &original, work.cancellation);
                document.root().to_json_with_work(&control, &mut work).await
            },
        )
        .unwrap();
        let mut rendered = None;
        for _ in 0..1_000_000 {
            if let Some(value) = output
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })
                .unwrap()
            {
                rendered = Some(value);
                break;
            }
        }
        assert_eq!(
            &**rendered.as_ref().expect("completed JSON output"),
            &expected
        );
        drop(rendered);
        drop(output);
        drop(input);
        assert_eq!(memory.used(), 0);
    }
    assert!(observations[0] > observations[1] && observations[1] > observations[2]);
}

#[test]
fn native_json_document_releases_failed_cancelled_and_abandoned_generations() {
    let bytes: Arc<[u8]> = format!(
        r#"{{"a":[{}],"z":"{}"}}"#,
        (0..128)
            .map(|n| format!("{{\"n\":{n}}}"))
            .collect::<Vec<_>>()
            .join(","),
        "x".repeat(32768)
    )
    .into_bytes()
    .into();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(8 << 20);
    let value = JsonDocument::read_with_control(
        &bytes,
        &ProductionControl::new(&memory, &token, &token),
        protocol(),
    )
    .unwrap();
    let peak = memory.peak();
    drop(value);
    assert_eq!(memory.used(), 0);
    for limit in (0..peak).step_by((peak / 12).max(1)) {
        let memory = MemoryBudget::new(limit);
        let error = JsonDocument::read_with_control(
            &bytes,
            &ProductionControl::new(&memory, &token, &token),
            protocol(),
        )
        .err()
        .expect("denied document allocation");
        assert!(matches!(error, JsonInputError::Memory(_)));
        drop(error);
        assert_eq!(memory.used(), 0);
    }
    for original_cancelled in [true, false] {
        let memory = MemoryBudget::new(8 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut pending = input(Arc::clone(&bytes), &memory, &original, &invoking);
        for _ in 0..50 {
            assert!(pending
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
                .is_none());
        }
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(pending
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
            .is_err());
        drop(pending);
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(8 << 20);
    let mut pending = input(bytes, &memory, &token, &token);
    for _ in 0..50 {
        assert!(pending
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
            .unwrap()
            .is_none());
    }
    drop(pending);
    assert_eq!(memory.used(), 0);
}
