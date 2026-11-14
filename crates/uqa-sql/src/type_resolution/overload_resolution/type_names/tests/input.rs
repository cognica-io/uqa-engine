//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::type_resolution::RoutineSignatureInputError;
use std::sync::Arc;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError};

type Decoder = ImageDecoder<Produced<String>, RoutineSignatureInputError>;

fn decoder(
    source: &str,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<Decoder, RoutineSignatureInputError> {
    let source: Arc<str> = source.into();
    let retained = memory.clone();
    let original = original.clone();
    Ok(ImageDecoder::new(
        memory,
        invoking,
        |mut reader| async move {
            let mut input = reader.input();
            let control = ProductionControl::new(&retained, &original, input.cancellation);
            canonical_routine_type_name_from_input(&source, &control, &mut input).await
        },
    )?)
}

struct NoSourceReads;
impl std::io::Read for NoSourceReads {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("native signature normalization must not consume source bytes")
    }
}

fn advance(
    input: &mut Decoder,
    steps: usize,
) -> Result<Option<Produced<String>>, RoutineSignatureInputError> {
    input.advance(&mut NoSourceReads, ImageDecodeWindow { bytes: 1, steps })
}

fn complete(
    input: &mut Decoder,
    steps: usize,
) -> Result<(Produced<String>, usize), RoutineSignatureInputError> {
    for poll in 1..100_000 {
        if let Some(value) = advance(input, steps)? {
            return Ok((value, poll));
        }
    }
    panic!("native type normalization stopped progressing")
}

#[test]
fn retained_routine_type_names_preserve_aliases_quotes_modifiers_and_array_identity() {
    for (source, expected) in [
        ("", ""),
        (" PG_CATALOG.INTEGER ", "int4"),
        ("\"pg_catalog\".\"integer\"[]", "\"integer\"[]"),
        ("\"pg_catalog\".\"int4\"", "int4"),
        ("numeric(10, -2) [] []", "numeric[]"),
        (
            "\u{2003}TIME ( 3 )  WITH TIME ZONE [][]\u{00a0}",
            "timetz[]",
        ),
        ("\"Custom (Type)\"", "\"custom (type)\""),
        ("\"a\"\"(b)\"(4)", "\"a\"\"(b)\""),
        ("x(one(two)) y", "x y"),
        ("x(1)y[]", "xy[]"),
        ("x(unclosed", "x"),
        ("x) y", "x) y"),
        ("PUBLIC.\"大きい 名\" (42) []", "public.\"大きい 名\"[]"),
        ("domain#4294967295[][]", "domain#4294967295[]"),
        ("\"pg_catalog\".\"Unknown Type\"(2)", "\"unknown type\""),
    ] {
        for steps in [1, 3, 64] {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = decoder(source, &memory, &token, &token).unwrap();
            let (value, _) = complete(&mut input, steps).unwrap();
            assert_eq!(value.as_str(), expected, "{source:?}");
            assert_eq!(canonical_routine_type_name(source), expected);
            drop(input);
            assert_eq!(memory.used(), value.capacity());
            assert_eq!(value.reserved_bytes(), value.capacity());
            drop(value);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn retained_routine_type_names_bound_whitespace_modifiers_quotes_and_suffix_work() {
    let fixtures = [
        (
            format!("{}INTEGER{}", "\u{2003}".repeat(8192), " ".repeat(8192)),
            "int4".into(),
        ),
        (format!("numeric({})", " ".repeat(8192)), "numeric".into()),
        (format!("numeric({})", "0".repeat(16384)), "numeric".into()),
        (format!("int{}", "[] ".repeat(8192)), "int4[]".into()),
        (
            format!("\"{}\"", "a\"\"".repeat(4096)),
            format!("\"{}\"", "a\"\"".repeat(4096)),
        ),
    ];
    for (source, expected) in fixtures {
        let memory = MemoryBudget::new(2 << 20);
        let token = CancellationToken::new();
        let mut input = decoder(&source, &memory, &token, &token).unwrap();
        assert!(advance(&mut input, 1).unwrap().is_none());
        let (value, polls) = complete(&mut input, 1).unwrap();
        assert_eq!(value.as_str(), expected);
        assert!(polls >= source.chars().count() / 1024, "{polls} polls");
        assert_eq!(canonical_routine_type_name(&source), expected);
        drop((value, input));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn retained_routine_type_names_retire_every_pending_transfer_and_admission_failure() {
    let source = format!("\"{}\"(12)[]", "名\"\" ".repeat(1024));
    let memory = MemoryBudget::new(2 << 20);
    let token = CancellationToken::new();
    let mut input = decoder(&source, &memory, &token, &token).unwrap();
    let (value, polls) = complete(&mut input, 1).unwrap();
    let peak = memory.peak();
    drop((value, input));
    assert_eq!(memory.used(), 0);
    for stop in 0..polls {
        for cancellation in [None, Some(false), Some(true)] {
            let memory = MemoryBudget::new(2 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = decoder(&source, &memory, &original, &invoking).unwrap();
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
                    Err(
                        RoutineSignatureInputError::Native(ValueRetentionError::Cancelled(_))
                            | RoutineSignatureInputError::Work(ImageIoError::Cancelled(_))
                    )
                ));
                assert_eq!(memory.used(), 0);
                assert!(
                    advance(&mut input, 2).is_err(),
                    "retired input stays terminal"
                );
            }
            drop(input);
            assert_eq!(memory.used(), 0);
        }
    }
    for limit in [0, 8, 512, peak / 2, peak - 1, peak, peak + 1] {
        let memory = MemoryBudget::new(limit);
        let result = decoder(&source, &memory, &token, &token)
            .and_then(|mut input| complete(&mut input, 3).map(|(value, _)| value));
        match result {
            Ok(value) => {
                assert_eq!(value.as_str(), canonical_routine_type_name(&source));
                assert_eq!(memory.used(), value.reserved_bytes());
                assert!(memory.peak() <= limit);
            }
            Err(
                RoutineSignatureInputError::Native(ValueRetentionError::Memory(_))
                | RoutineSignatureInputError::Work(ImageIoError::Memory(_)),
            ) => {
                assert!(limit < peak);
            }
            Err(error) => panic!("unexpected admission failure: {error}"),
        }
        assert_eq!(memory.used(), 0);
    }
}
