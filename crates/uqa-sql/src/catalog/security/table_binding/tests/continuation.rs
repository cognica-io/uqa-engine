//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{roles, security, BoundTableSecurity, RoleDefinitions};
use crate::SQLError;
use std::{io::Read, sync::Arc};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

struct NoRead;
impl Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("authority validation must not read or replay serialized input")
    }
}

fn input(
    value: &Arc<BoundTableSecurity>,
    roles: &Arc<RoleDefinitions>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<ImageDecoder<(), SQLError>, SQLError> {
    let value = value.clone();
    let roles = roles.clone();
    let invoking = invoking.clone();
    Ok(ImageDecoder::new(
        memory,
        original,
        |mut reader| async move {
            let mut input = reader.input();
            let control = ProductionControl::new(input.memory, input.cancellation, &invoking);
            value
                .validate_from_input(Some(["id"].into_iter()), &roles, &control, &mut input)
                .await
        },
    )?)
}

fn advance(input: &mut ImageDecoder<(), SQLError>, steps: usize) -> Result<Option<()>, SQLError> {
    input.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })
}

fn finish(input: &mut ImageDecoder<(), SQLError>, steps: usize) -> Result<usize, SQLError> {
    let mut pending = 0;
    while advance(input, steps)?.is_none() {
        pending += 1;
        assert!(pending < 10_000);
    }
    Ok(pending)
}

#[test]
fn retained_authority_validation_resumes_original_roles_and_grant_closure() {
    let roles = Arc::new(roles());
    let value = Arc::new(security(&roles));
    let expected = (*value).clone();
    let token = CancellationToken::new();
    for steps in [1, 3, 64] {
        let memory = MemoryBudget::new(1 << 20);
        let mut input = input(&value, &roles, &memory, &token, &token).unwrap();
        let calls = finish(&mut input, steps).unwrap();
        if steps == 1 {
            assert!(calls > 64, "role and grant visits must yield");
        }
        assert_eq!(*value, expected);
        assert_eq!(Arc::strong_count(&value), 1);
        assert_eq!(Arc::strong_count(&roles), 1);
        assert_eq!(memory.used(), 0);
    }
    for defect in ["revision", "grantor", "column"] {
        let mut value = expected.clone();
        let diagnostic = match defect {
            "revision" => {
                value.acl_revisions.relation = Some([0; 16]);
                value.role_owner.object_id = [0; 16];
                "invalid relation or attribute ACL tuple identity"
            }
            "grantor" => {
                value.column_acls.get_mut("id").unwrap()[0]
                    .grantor
                    .object_id = [0; 16];
                "invalid persisted table role identity"
            }
            "column" => {
                value.column_acls.get_mut("id").unwrap()[0]
                    .privileges
                    .delete = true;
                "column ACL for `id` contains a relation-only privilege"
            }
            _ => unreachable!(),
        };
        let memory = MemoryBudget::new(1 << 20);
        let mut input = input(&Arc::new(value), &roles, &memory, &token, &token).unwrap();
        assert!(
            matches!(finish(&mut input, 1), Err(SQLError::Internal(message)) if message == diagnostic)
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn retained_authority_validation_retires_scratch_at_every_pending_boundary() {
    let roles = Arc::new(roles());
    let value = Arc::new(security(&roles));
    let token = CancellationToken::new();
    let baseline = MemoryBudget::new(1 << 20);
    let pending = finish(
        &mut input(&value, &roles, &baseline, &token, &token).unwrap(),
        1,
    )
    .unwrap();
    let peak = baseline.peak();
    assert_eq!(baseline.used(), 0);
    for stop in 0..=pending {
        for cancel in [None, Some(false), Some(true)] {
            let memory = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = input(&value, &roles, &memory, &original, &invoking).unwrap();
            for _ in 0..stop {
                assert!(advance(&mut input, 1).unwrap().is_none());
            }
            if let Some(original_owner) = cancel {
                if original_owner {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert!(matches!(
                    advance(&mut input, 1),
                    Err(SQLError::Cancelled(_))
                ));
            }
            drop(input);
            assert_eq!(memory.used(), 0, "stop {stop}, cancel {cancel:?}");
            assert_eq!(Arc::strong_count(&value), 1);
            assert_eq!(Arc::strong_count(&roles), 1);
        }
    }
    for limit in (0..peak).step_by(257).chain([peak - 1]) {
        let memory = MemoryBudget::new(limit);
        let result = input(&value, &roles, &memory, &token, &token)
            .and_then(|mut input| finish(&mut input, 1));
        assert_eq!(
            result.unwrap_err().sqlstate(),
            Some("53200"),
            "{limit}/{peak}"
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn retained_authority_validation_copies_utf8_role_names_in_bounded_fragments() {
    let mut roles = roles();
    let value = Arc::new(security(&roles));
    let mut owner = roles["owner"].clone();
    roles.remove("owner").unwrap();
    owner.name = "주인🦆".repeat(2048);
    roles.insert(owner.name.clone(), owner).unwrap();
    let roles = Arc::new(roles);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = input(&value, &roles, &memory, &token, &token).unwrap();
    assert!(finish(&mut input, 1).unwrap() > 100);
    value.validate(Some(&["id".into()]), &roles).unwrap();
    assert_eq!(memory.used(), 0);
}
