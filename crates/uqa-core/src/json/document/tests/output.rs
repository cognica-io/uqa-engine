//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::image_io::{ImageDecodeWindow, ImageDecoder};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("output must borrow the completed document")
    }
}

fn source(memory: &MemoryBudget) -> JsonDocument {
    let text = format!(
        r#"{{"x":[{},{{"a":"한🙂\u0000\n"}}],"z":null}}"#,
        "1,".repeat(24).trim_end_matches(',')
    );
    let token = CancellationToken::new();
    JsonDocument::read_with_control(
        text.as_bytes(),
        &ProductionControl::new(memory, &token, &token),
        protocol(),
    )
    .unwrap()
}

fn output(
    document: JsonDocument,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<ImageDecoder<Budgeted<String>, JsonInputError>, JsonInputError> {
    let retained = memory.clone();
    let original = original.clone();
    Ok(ImageDecoder::new(
        memory,
        invoking,
        |mut channel| async move {
            let mut work = channel.input();
            let control = ProductionControl::new(&retained, &original, work.cancellation);
            document.root().to_json_with_work(&control, &mut work).await
        },
    )?)
}

fn advance(
    output: &mut ImageDecoder<Budgeted<String>, JsonInputError>,
) -> Result<Option<Budgeted<String>>, JsonInputError> {
    output.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
}

#[test]
fn native_json_document_output_retires_each_interrupted_or_denied_generation() {
    let source_memory = MemoryBudget::new(1 << 20);
    let output_memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut completed = output(source(&source_memory), &output_memory, &token, &token).unwrap();
    let mut polls = 0;
    let expected = loop {
        polls += 1;
        assert!(polls < 10000);
        if let Some(text) = advance(&mut completed).unwrap() {
            assert!(text.contains("한🙂\\u0000\\n"));
            break text.to_string();
        }
    };
    drop(completed);
    assert_eq!(source_memory.used(), 0);
    assert_eq!(output_memory.used(), 0);
    for boundary in 1..polls {
        for cancel in [None, Some(true), Some(false)] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut pending =
                output(source(&source_memory), &output_memory, &original, &invoking).unwrap();
            for _ in 0..boundary {
                assert!(advance(&mut pending).unwrap().is_none());
            }
            if let Some(original_owner) = cancel {
                if original_owner {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert!(matches!(
                    advance(&mut pending),
                    Err(JsonInputError::Cancelled(_))
                ));
            }
            drop(pending);
            assert_eq!(source_memory.used(), 0, "boundary {boundary}");
            assert_eq!(output_memory.used(), 0, "boundary {boundary}");
        }
    }
    let mut denied = 0;
    let mut accepted = 0;
    for limit in (0..output_memory.peak() + 32).step_by(31) {
        let memory = MemoryBudget::new(limit);
        let result =
            output(source(&source_memory), &memory, &token, &token).and_then(|mut output| loop {
                if let Some(value) = advance(&mut output)? {
                    break Ok(value);
                }
            });
        match &result {
            Err(JsonInputError::Memory(_)) => denied += 1,
            // Native buffers may choose exact-capacity growth under a smaller allowance.
            Ok(value) => {
                accepted += 1;
                assert_eq!(&**value, &expected, "limit {limit}");
            }
            Err(error) => panic!("limit {limit}: {error}"),
        }
        assert!(memory.peak() <= limit);
        drop(result);
        assert_eq!(memory.used(), 0);
        assert_eq!(source_memory.used(), 0);
    }
    assert!(denied > 0 && accepted > 0);
}
