//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

type Output = Produced<Option<Vec<LegacyRoutineAclEntry>>>;
type Decoder = ImageDecoder<Output, JsonInputError>;

fn input(
    text: &str,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> (Result<Decoder, JsonInputError>, MemoryBudget) {
    let source_memory = MemoryBudget::new(8 << 20);
    let document = JsonDocument::read_with_control(
        text.as_bytes(),
        &ProductionControl::new(&source_memory, original, invoking),
        protocol(),
    )
    .unwrap();
    let retained = memory.clone();
    let original = original.clone();
    let input = ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&retained, &original, work.cancellation);
        read_legacy_acl_with_work(Some(document.root()), &control, &mut work).await
    });
    (input.map_err(Into::into), source_memory)
}

fn advance(input: &mut Decoder, steps: usize) -> Result<Option<Output>, JsonInputError> {
    input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
}

fn finish(input: &mut Decoder, steps: usize) -> Result<(Output, usize), JsonInputError> {
    for count in 1..65536 {
        if let Some(value) = advance(input, steps)? {
            return Ok((value, count));
        }
    }
    panic!("legacy routine ACL input stopped progressing")
}

#[test]
fn projected_legacy_routine_acl_resumes_and_retires_all_interrupted_native_owners() {
    let text = format!(
        r#"[{{"role":{{"kind":"role","name":"{}"}},"grantor":"{}","grant_option":false}}]"#,
        "한🙂".repeat(8192),
        "grantor🙂".repeat(8192)
    );
    let expected = serde_json::from_str::<Option<Vec<LegacyRoutineAclEntry>>>(&text).unwrap();
    let token = CancellationToken::new();
    let mut counts = Vec::new();
    for steps in [1, 7, 64] {
        let memory = MemoryBudget::new(8 << 20);
        let (decoder, source_memory) = input(&text, &memory, &token, &token);
        let mut decoder = decoder.unwrap();
        let (output, polls) = finish(&mut decoder, steps).unwrap();
        counts.push(polls);
        assert_eq!(source_memory.used(), 0);
        equal(output.as_deref(), expected.as_deref());
        assert!(advance(&mut decoder, 1).is_err());
        drop(output);
        drop(decoder);
        assert_eq!(memory.used(), 0);
    }
    assert!(counts[0] > counts[1] && counts[1] > counts[2]);
    for boundary in [0, 1, counts[0] / 2, counts[0] - 1] {
        for outcome in ["original", "invoking", "drop", "zero"] {
            let memory = MemoryBudget::new(8 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let (decoder, source_memory) = input(&text, &memory, &original, &invoking);
            let mut decoder = decoder.unwrap();
            for _ in 0..boundary {
                assert!(advance(&mut decoder, 1).unwrap().is_none());
            }
            match outcome {
                "original" => original.cancel(),
                "invoking" => invoking.cancel(),
                _ => (),
            }
            if outcome == "zero" {
                assert!(advance(&mut decoder, 0).is_err());
            }
            if matches!(outcome, "original" | "invoking") {
                assert!(matches!(
                    advance(&mut decoder, 1),
                    Err(JsonInputError::Cancelled(_))
                ));
                assert!(advance(&mut decoder, 1).is_err());
            }
            drop(decoder);
            assert_eq!(memory.used(), 0);
            assert_eq!(source_memory.used(), 0);
        }
    }
    let mut accepted = 0;
    let mut rejected = 0;
    for limit in [0, 1024, 4096, 16_384, 65_536, 131_072, 262_144, 8 << 20] {
        let memory = MemoryBudget::new(limit);
        let (decoder, source_memory) = input(&text, &memory, &token, &token);
        let result =
            decoder.and_then(|mut decoder| finish(&mut decoder, 7).map(|(value, _)| value));
        match &result {
            Ok(value) => {
                accepted += 1;
                equal(value.as_deref(), expected.as_deref());
            }
            Err(JsonInputError::Memory(_)) => rejected += 1,
            Err(error) => panic!("limit {limit}: {error}"),
        }
        assert!(memory.peak() <= limit);
        drop(result);
        assert_eq!(memory.used(), 0);
        assert_eq!(source_memory.used(), 0);
    }
    assert!(accepted > 0 && rejected > 0);
}
