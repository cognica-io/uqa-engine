//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{sync::Arc, task::Poll};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

type Decoder = ImageDecoder<Option<UserTypeIdentity>, TypeIdentityInputError>;

struct NoSourceReads;
impl std::io::Read for NoSourceReads {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("native identity parsing does not reread its source")
    }
}

fn decoder(
    name: Arc<str>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Decoder {
    let original = original.clone();
    let native = memory.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        UserTypeIdentity::parse_from_input(&name, &control, &mut input).await
    })
    .unwrap()
}
fn advance(
    input: &mut Decoder,
    steps: usize,
) -> Result<Poll<Option<UserTypeIdentity>>, TypeIdentityInputError> {
    input
        .advance(&mut NoSourceReads, ImageDecodeWindow { bytes: 1, steps })
        .map(|value| value.map_or(Poll::Pending, Poll::Ready))
}
fn complete(input: &mut Decoder, steps: usize) -> (Option<UserTypeIdentity>, usize) {
    for polls in 1..100_000 {
        if let Poll::Ready(value) = advance(input, steps).unwrap() {
            return (value, polls);
        }
    }
    panic!("identity input stopped progressing")
}

#[test]
fn retained_type_identity_parsing_matches_original_whitespace_decimal_and_array_grammar() {
    #[derive(serde::Deserialize)]
    struct Reference {
        source: String,
        observations: Vec<Observation>,
    }
    #[derive(serde::Deserialize)]
    struct Observation {
        input: String,
        expected: Option<Expected>,
    }
    #[derive(serde::Deserialize)]
    struct Expected {
        kind: String,
        oid: u32,
        dimensions: usize,
    }
    let reference: Reference = serde_json::from_str(include_str!("reference.json")).unwrap();
    assert_eq!(reference.source, "9ced2fd251e2106db1b464a45712b5e712e28e59");
    assert_eq!(reference.observations.len(), 745);
    for observation in reference.observations {
        let expected = observation.expected.map(|value| UserTypeIdentity {
            kind: match value.kind.as_str() {
                "Enum" => UserTypeKind::Enum,
                "Domain" => UserTypeKind::Domain,
                "Composite" => UserTypeKind::Composite,
                "Range" => UserTypeKind::Range,
                "Multirange" => UserTypeKind::Multirange,
                _ => panic!("invalid original observation"),
            },
            oid: value.oid,
            dimensions: value.dimensions,
        });
        let mut ordinary = None;
        let allocation = allocation_counter::measure(|| {
            ordinary = Some(UserTypeIdentity::parse(&observation.input));
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(ordinary.unwrap(), expected, "{:?}", observation.input);
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let name: Arc<str> = observation.input.into();
        for steps in [1, 3, 128] {
            let mut input = decoder(Arc::clone(&name), &memory, &token, &token);
            let mut actual = None;
            let allocation =
                allocation_counter::measure(|| actual = Some(complete(&mut input, steps).0));
            assert_eq!(allocation.count_total, 0);
            assert_eq!(actual.unwrap(), expected);
            assert_eq!(memory.used(), 0);
            assert!(advance(&mut input, steps).is_err());
        }
    }
}

#[test]
fn retained_type_identity_parsing_bounds_long_zeroes_dimensions_and_unicode_whitespace() {
    let name: Arc<str> = format!(
        "{}enum#{}42{}{}",
        "\u{2000}".repeat(8193),
        "0".repeat(32_769),
        "[]".repeat(4097),
        "\u{3000}".repeat(8193)
    )
    .into();
    let expected = Some(UserTypeIdentity {
        kind: UserTypeKind::Enum,
        oid: 42,
        dimensions: 4097,
    });
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    assert_eq!(UserTypeIdentity::parse(&name), expected);
    let (actual, polls) = complete(&mut decoder(Arc::clone(&name), &memory, &token, &token), 1);
    assert_eq!(actual, expected);
    assert!(polls >= 50);
    for pending in 0..polls - 1 {
        for action in ["original", "invoking", "drop", "window"] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = decoder(Arc::clone(&name), &memory, &original, &invoking);
            for _ in 0..pending {
                assert!(advance(&mut input, 1).unwrap().is_pending());
            }
            match action {
                "original" => original.cancel(),
                "invoking" => invoking.cancel(),
                _ => {}
            }
            if action != "drop" {
                assert!(advance(&mut input, usize::from(action != "window")).is_err());
            }
            drop(input);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn retained_type_identity_rendering_retains_native_allowance_and_exact_suffixes() {
    for kind in [
        UserTypeKind::Enum,
        UserTypeKind::Domain,
        UserTypeKind::Composite,
        UserTypeKind::Range,
        UserTypeKind::Multirange,
    ] {
        let identity = UserTypeIdentity {
            kind,
            oid: u32::MAX,
            dimensions: 4097,
        };
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let comparison = MemoryBudget::new(1 << 20);
        let expected = identity
            .render_with_control(&ProductionControl::new(&comparison, &token, &token))
            .unwrap();
        let budget = native.clone();
        let original = token.clone();
        let mut input = ImageDecoder::<_, TypeIdentityInputError>::new(
            &work,
            &token,
            |mut reader| async move {
                let mut input = reader.input();
                identity
                    .render_from_input(
                        &ProductionControl::new(&budget, &original, input.cancellation),
                        &mut input,
                    )
                    .await
            },
        )
        .unwrap();
        let mut polls = 0;
        let actual = loop {
            polls += 1;
            if let Some(value) = input
                .advance(&mut NoSourceReads, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
            {
                break value;
            }
        };
        assert!(polls >= 5);
        assert_eq!(*actual, *expected);
        assert_eq!(actual.capacity(), expected.capacity());
        assert_eq!(native.used(), actual.capacity());
        assert_eq!(work.used(), 0);
        token.cancel();
        assert_eq!(UserTypeIdentity::parse(&actual), Some(identity));
        drop((actual, input, expected));
        assert_eq!(native.used(), 0);
        assert_eq!(comparison.used(), 0);
    }
}
