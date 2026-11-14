//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::{Budgeted, MemoryBudget},
    CancellationToken, RelationIdentity,
};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("canonical name rendering must not read source bytes")
    }
}

fn input(
    name: String,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> ImageDecoder<Budgeted<String>> {
    ImageDecoder::new(memory, cancellation, |mut reader| async move {
        RelationIdentity::borrowed_reference(&name)
            .unwrap()
            .render_from(&mut reader.input())
            .await
            .map_err(|error| match error {
                ValueRetentionError::Memory(error) => ImageIoError::Memory(error),
                ValueRetentionError::Cancelled(error) => ImageIoError::Cancelled(error),
                error @ ValueRetentionError::Malformed { .. } => {
                    panic!("valid relation name failed: {error}")
                }
            })
    })
    .unwrap()
}

#[test]
fn retained_relation_rendering_resumes_quoted_components_with_exact_native_capacity() {
    let cancellation = CancellationToken::new();
    for (source, expected) in [
        ("public.table".into(), "public.table".into()),
        ("\"foo\".\"bar\"".into(), "foo.bar".into()),
        (
            "\"A.Schema\".\"a\"\"b\"".into(),
            "\"A.Schema\".\"a\"\"b\"".into(),
        ),
        ("\"한국어\"".into(), "\"한국어\"".into()),
        (
            format!("\"{}\".\"{}\"", "a\"\"한🙂".repeat(4096), "z".repeat(16384)),
            format!("\"{}\".{}", "a\"\"한🙂".repeat(4096), "z".repeat(16384)),
        ),
    ] {
        let ordinary_memory = MemoryBudget::new(2 << 20);
        let control = ProductionControl::new(&ordinary_memory, &cancellation, &cancellation);
        let ordinary = RelationIdentity::borrowed_reference(&source)
            .unwrap()
            .render_with_control(&control)
            .unwrap();
        let memory = MemoryBudget::new(2 << 20);
        let mut input = input(source, &memory, &cancellation);
        let mut calls = 0;
        let value = loop {
            calls += 1;
            if let Some(value) = input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
            {
                break value;
            }
        };
        assert_eq!(*value, expected);
        assert_eq!(*value, *ordinary);
        assert_eq!(value.capacity(), ordinary.capacity());
        assert_eq!(value.reserved_bytes(), ordinary_memory.used());
        assert!(calls >= expected.chars().count().div_ceil(1024));
        drop((value, input, ordinary));
        assert_eq!(memory.used(), 0);
        assert_eq!(ordinary_memory.used(), 0);
    }
}

#[test]
fn retained_relation_rendering_retires_partial_names_on_every_terminal_path() {
    for failure in ["cancel", "memory", "window", "drop"] {
        let memory = MemoryBudget::new(2 << 20);
        let cancellation = CancellationToken::new();
        let mut input = input(
            format!("\"{}\".item", "한\"\"🙂".repeat(8192)),
            &memory,
            &cancellation,
        );
        for _ in 0..36 {
            assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
                .is_none());
        }
        let full = (failure == "memory").then(|| memory.reserve(memory.available()).unwrap());
        if failure == "cancel" {
            cancellation.cancel();
        }
        if failure != "drop" {
            let mut failed = false;
            for _ in 0..128 {
                match input.advance(
                    &mut NoRead,
                    ImageDecodeWindow {
                        bytes: 1,
                        steps: usize::from(failure != "window"),
                    },
                ) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("failed name rendering published: {failure}"),
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            assert!(failed, "{failure}");
            assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .is_err());
        }
        drop((input, full));
        assert_eq!(memory.used(), 0, "{failure}");
    }
}

fn identity_input(
    source: String,
    default_schema: String,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<Produced<RelationIdentity>> {
    let retained = memory.clone();
    let original = original.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut input = reader.input();
        let reference = match RelationIdentity::try_borrowed_reference_from_input(
            &source, &original, &mut input,
        )
        .await
        {
            Ok(reference) => reference,
            Err(crate::RelationReferenceInputError::Input(error)) => return Err(error),
            Err(error) => panic!("valid identity failed: {error}"),
        };
        let control = ProductionControl::new(&retained, &original, input.cancellation);
        reference
            .identity_from_input(RelationName::decoded(&default_schema), &control, &mut input)
            .await
            .map_err(|error| match error {
                ValueRetentionError::Memory(error) => ImageIoError::Memory(error),
                ValueRetentionError::Cancelled(error) => ImageIoError::Cancelled(error),
                error @ ValueRetentionError::Malformed { .. } => {
                    panic!("valid identity failed: {error}")
                }
            })
    })
    .unwrap()
}

fn identity_complete(
    input: &mut ImageDecoder<Produced<RelationIdentity>>,
) -> Result<(Produced<RelationIdentity>, usize), ImageIoError> {
    for poll in 1..100_000 {
        if let Some(value) = input.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })? {
            return Ok((value, poll));
        }
    }
    panic!("retained identity stopped progressing")
}

#[test]
fn retained_relation_identity_preserves_default_schema_escaped_components_and_native_ownership() {
    for (source, default_schema, expected) in [
        (
            "public.f".into(),
            "different".to_owned(),
            RelationIdentity::new("public", "f"),
        ),
        (
            "\"a\"\"b\"".into(),
            "default.schema".to_owned(),
            RelationIdentity::new("default.schema", "a\"b"),
        ),
        (
            format!(
                "\"{}\".\"{}\"",
                "스키마\"\"".repeat(2048),
                "名".repeat(4096)
            ),
            "unused".to_owned(),
            RelationIdentity::new("스키마\"".repeat(2048), "名".repeat(4096)),
        ),
    ] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let reference = RelationIdentity::try_borrowed_reference(&source).unwrap();
        let ordinary = reference
            .identity_with_control(
                RelationName::decoded(&default_schema),
                &ProductionControl::uncontrolled(),
            )
            .unwrap();
        assert_eq!(*ordinary, expected);
        let mut input = identity_input(source, default_schema, &memory, &original, &invoking);
        let (value, _) = identity_complete(&mut input).unwrap();
        assert_eq!(*value, expected);
        assert_eq!(
            value.reserved_bytes(),
            value.schema.capacity() + value.name.capacity()
        );
        drop(input);
        original.cancel();
        invoking.cancel();
        assert_eq!(*value, expected);
        assert_eq!(memory.used(), value.reserved_bytes());
        drop(value);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn retained_relation_identity_retires_every_partial_parse_or_component_transfer() {
    let source = format!("\"{}\".\"{}\"", "한\"\"".repeat(1024), "名".repeat(2048));
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (value, polls) = identity_complete(&mut identity_input(
        source.clone(),
        "public".into(),
        &memory,
        &token,
        &token,
    ))
    .unwrap();
    let peak = memory.peak();
    drop(value);
    assert_eq!(memory.used(), 0);
    for stop in 0..polls {
        for cancellation in [None, Some(false), Some(true)] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = identity_input(
                source.clone(),
                "public".into(),
                &memory,
                &original,
                &invoking,
            );
            for _ in 0..stop {
                assert!(input
                    .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                    .unwrap()
                    .is_none());
            }
            if let Some(cancel_original) = cancellation {
                if cancel_original {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert!(matches!(
                    identity_complete(&mut input),
                    Err(ImageIoError::Cancelled(_))
                ));
                assert_eq!(memory.used(), 0);
            }
            drop(input);
            assert_eq!(memory.used(), 0);
        }
    }
    for limit in [peak / 2, peak - 1, peak] {
        let memory = MemoryBudget::new(limit);
        let result = identity_complete(&mut identity_input(
            source.clone(),
            "public".into(),
            &memory,
            &token,
            &token,
        ));
        match result {
            Ok((value, _)) => {
                assert_eq!(*value, RelationIdentity::from_legacy_name(&source).unwrap());
                assert!(memory.peak() <= limit);
            }
            Err(ImageIoError::Memory(_)) => assert!(limit < peak),
            Err(error) => panic!("unexpected identity admission failure: {error}"),
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn retained_empty_relation_identity_observes_cancelled_input_admission() {
    let memory = MemoryBudget::new(1 << 20);
    let retained = memory.clone();
    let invoking = CancellationToken::new();
    let cancelled = invoking.clone();
    let original = CancellationToken::new();
    let mut input = ImageDecoder::new(&memory, &invoking, |mut reader| async move {
        let control = ProductionControl::new(&retained, &original, &original);
        let mut input = reader.input();
        cancelled.cancel();
        RelationReference {
            schema: None,
            name: RelationName::decoded(""),
        }
        .identity_from_input(RelationName::decoded(""), &control, &mut input)
        .await
        .map_err(|error| match error {
            ValueRetentionError::Memory(error) => ImageIoError::Memory(error),
            ValueRetentionError::Cancelled(error) => ImageIoError::Cancelled(error),
            error @ ValueRetentionError::Malformed { .. } => {
                panic!("native empty identity failed: {error}")
            }
        })
    })
    .unwrap();
    assert!(matches!(
        input.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 }),
        Err(ImageIoError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}
