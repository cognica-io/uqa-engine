//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
};
use std::sync::Arc;

#[derive(serde::Deserialize)]
struct Case {
    input: String,
    schema: Option<String>,
    name: Option<String>,
    error: Option<String>,
}

fn compare(case: &Case, value: Result<RelationReference<'_>, RelationReferenceError<'_>>) {
    match value {
        Ok(reference) => {
            assert!(case.error.is_none(), "{}", case.input);
            assert_eq!(reference.schema.is_some(), case.schema.is_some());
            if let Some(schema) = reference.schema {
                assert!(schema.chars().eq(case.schema.as_ref().unwrap().chars()));
            }
            assert!(reference
                .name
                .chars()
                .eq(case.name.as_ref().unwrap().chars()));
        }
        Err(error) => assert_eq!(Some(error.to_string()), case.error),
    }
}

type Decoder = ImageDecoder<()>;
fn decoder(
    case: Arc<Case>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Decoder {
    let original = original.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut input = reader.input();
        match RelationIdentity::try_borrowed_reference_from_input(
            &case.input,
            &original,
            &mut input,
        )
        .await
        {
            Ok(reference) => compare(&case, Ok(reference)),
            Err(RelationReferenceInputError::Reference(error)) => compare(&case, Err(error)),
            Err(RelationReferenceInputError::Input(error)) => return Err(error),
        }
        Ok(())
    })
    .unwrap()
}

struct NoSourceReads;
impl std::io::Read for NoSourceReads {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("borrowed native names do not consume source bytes")
    }
}
fn advance(input: &mut Decoder, steps: usize) -> Result<Option<()>, ImageIoError> {
    input.advance(&mut NoSourceReads, ImageDecodeWindow { bytes: 1, steps })
}
fn complete(input: &mut Decoder, steps: usize) -> Result<usize, ImageIoError> {
    for poll in 1..100_000 {
        if advance(input, steps)?.is_some() {
            return Ok(poll);
        }
    }
    panic!("borrowed reference input stopped progressing")
}

#[test]
fn retained_reference_names_match_original_parser_results_and_exact_diagnostics() {
    // Captured by compiling the unchanged standard-library-only parser from 45d9070299fa7ae34f931ae0aa9f323d44a418e6, before adding retained work.
    let cases: Vec<Case> = serde_json::from_str(include_str!("reference_names.json")).unwrap();
    assert_eq!(cases.len(), 745);
    for case in cases {
        compare(&case, RelationIdentity::try_borrowed_reference(&case.input));
        let case = Arc::new(case);
        for steps in [1, 3, 32] {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = decoder(Arc::clone(&case), &memory, &token, &token);
            complete(&mut input, steps).unwrap();
            assert_eq!(memory.used(), 0);
            assert!(
                advance(&mut input, steps).is_err(),
                "completed input is terminal"
            );
        }
    }
}

#[test]
fn retained_reference_names_bound_quoted_and_unquoted_work_without_allocating_names() {
    for quoted in [false, true] {
        let schema = "aé".repeat(8192);
        let name = if quoted {
            "b\"名".repeat(8192)
        } else {
            "b名".repeat(8192)
        };
        let source = if quoted {
            format!("\"{schema}\".\"{}\"", name.replace('"', "\"\""))
        } else {
            format!("{schema}.{name}")
        };
        let case = Arc::new(Case {
            input: source,
            schema: Some(schema),
            name: Some(name),
            error: None,
        });
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = decoder(Arc::clone(&case), &memory, &token, &token);
        let mut polls = 0;
        loop {
            polls += 1;
            let mut result = None;
            let measured = allocation_counter::measure(|| result = Some(advance(&mut input, 1)));
            assert_eq!(measured.count_total, 0);
            if result.unwrap().unwrap().is_some() {
                break;
            }
            assert!(polls < 100_000);
        }
        assert!(polls >= case.input.len() / 1025);
        assert_eq!(memory.used(), 0);
        let mut reference = None;
        let measured = allocation_counter::measure(|| {
            reference = Some(RelationIdentity::try_borrowed_reference(&case.input));
        });
        assert_eq!(measured.count_total, 0);
        compare(&case, reference.unwrap());
    }
}

#[test]
fn retained_reference_names_retire_every_pending_parse_and_both_cancellation_owners() {
    let name = "x\"字".repeat(1024);
    let case = Arc::new(Case {
        input: format!("public.\"{}\"", name.replace('"', "\"\"")),
        schema: Some("public".into()),
        name: Some(name),
        error: None,
    });
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let polls = complete(&mut decoder(Arc::clone(&case), &memory, &token, &token), 1).unwrap();
    assert_eq!(memory.used(), 0);
    for stop in 0..polls {
        for cancellation in [None, Some(false), Some(true)] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = decoder(Arc::clone(&case), &memory, &original, &invoking);
            for _ in 0..stop {
                assert!(advance(&mut input, 1).unwrap().is_none());
            }
            if let Some(cancel_original) = cancellation {
                if cancel_original {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert!(matches!(
                    advance(&mut input, 2),
                    Err(ImageIoError::Cancelled(_))
                ));
                assert_eq!(memory.used(), 0);
                assert!(advance(&mut input, 2).is_err());
            }
            drop(input);
            assert_eq!(memory.used(), 0);
        }
    }
}
