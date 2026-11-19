//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::{identity::RoleSubject, RoleIdentity};
use std::{sync::Arc, task::Poll};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

type Decoder = ImageDecoder<Option<RoleIdentity>, SQLError>;
fn decoder(
    roles: RoleDefinitions,
    subject: RoleReference,
    work: &MemoryBudget,
    tokens: [&CancellationToken; 2],
) -> Decoder {
    let original = tokens[0].clone();
    ImageDecoder::new(work, tokens[1], |mut reader| async move {
        let mut input = reader.input();
        let native = MemoryBudget::new(0);
        subject
            .role_definition_from_input(
                &roles,
                &ProductionControl::new(&native, &original, input.cancellation),
                &mut input,
            )
            .await
            .map(|role| role.map(RoleDefinition::identity))
    })
    .unwrap()
}
fn advance(input: &mut Decoder) -> Result<Poll<Option<RoleIdentity>>, SQLError> {
    input
        .advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 },
        )
        .map(|value| value.map_or(Poll::Pending, Poll::Ready))
}
fn complete(input: &mut Decoder) -> (Option<RoleIdentity>, usize) {
    for polls in 1..100_000 {
        if let Poll::Ready(value) = advance(input).unwrap() {
            return (value, polls);
        }
    }
    panic!("native role selection stopped progressing")
}

#[test]
fn retained_role_lookup_preserves_long_named_order_and_renamed_bound_incarnations() {
    let name = format!("{}original", "가".repeat(8193));
    let mut original = RoleDefinition::bootstrap();
    original.name.clone_from(&name);
    original.oid = 42;
    original.object_id = [42; 16];
    let bound = RoleReference::Bound(Arc::new(RoleBinding::from_definition(&original).unwrap()));
    let initial = RoleDefinitions::try_from_iter([(name.clone(), original.clone())]).unwrap();
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for variant in 0..4 {
        let mut roles = initial.clone();
        if variant > 0 {
            roles.remove(&name).unwrap();
            if variant < 3 {
                let mut renamed = original.clone();
                renamed.name = "renamed".into();
                roles.insert(renamed.name.clone(), renamed).unwrap();
            }
            if variant > 1 {
                let mut replacement = original.clone();
                replacement.object_id = [43; 16];
                roles.insert(name.clone(), replacement).unwrap();
            }
        }
        for subject in [
            RoleReference::from(name.clone()),
            bound.clone(),
            "missing".into(),
        ] {
            let expected = subject
                .role_definition(&roles)
                .map(RoleDefinition::identity);
            let mut input = decoder(roles.clone(), subject.clone(), &work, [&token, &token]);
            let (actual, polls) = complete(&mut input);
            assert_eq!(actual, expected, "variant {variant}");
            if variant == 0 && subject != RoleReference::from("missing") {
                assert!(polls >= name.len() / 4096);
            }
            assert!(advance(&mut input).is_err());
            assert_eq!(work.used(), 0);
        }
    }
}

#[test]
fn retained_role_lookup_observes_both_cancellation_owners_even_for_empty_catalogs() {
    for original_cancelled in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let work = MemoryBudget::new(1 << 20);
        let mut input = decoder(
            RoleDefinitions::new(),
            "missing".into(),
            &work,
            [&original, &invoking],
        );
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(advance(&mut input).unwrap_err().sqlstate(), Some("57014"));
        assert!(advance(&mut input).is_err());
        assert_eq!(work.used(), 0);
    }
}
