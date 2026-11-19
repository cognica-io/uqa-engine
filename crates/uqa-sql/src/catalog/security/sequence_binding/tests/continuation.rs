//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{roles, security, BoundSequenceSecurity, RoleDefinitions};
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
    value: &Arc<BoundSequenceSecurity>,
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
                .validate_from_input(&roles, &control, &mut input)
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
fn retained_sequence_authority_validation_resumes_original_roles_and_grant_closure() {
    let roles = Arc::new(roles());
    let value = Arc::new(security(&roles));
    let expected = (*value).clone();
    let token = CancellationToken::new();
    let mut pending = Vec::new();
    for steps in [1, 3, 64] {
        let memory = MemoryBudget::new(1 << 20);
        let mut input = input(&value, &roles, &memory, &token, &token).unwrap();
        let calls = finish(&mut input, steps).unwrap();
        pending.push(calls);
        assert_eq!(*value, expected);
        assert_eq!(Arc::strong_count(&value), 1);
        assert_eq!(Arc::strong_count(&roles), 1);
        assert_eq!(memory.used(), 0);
    }
    assert!(pending[0] > pending[1] && pending[1] > pending[2]);
    for defect in ["owner", "grantor", "duplicate", "empty", "public"] {
        let mut value = expected.clone();
        let acl = value.acl.as_mut().unwrap();
        let diagnostic = match defect {
            "owner" => {
                value.role_owner.object_id = [0; 16];
                "invalid persisted sequence role identity"
            }
            "grantor" => {
                acl.push(acl[0].clone());
                acl[1].grantor.object_id = [0; 16];
                // Complete endpoint resolution precedes the later duplicate-path check.
                "invalid persisted sequence role identity"
            }
            "duplicate" => {
                acl.push(acl[0].clone());
                "ACL contains duplicate grant path `owner` -> `grantor`"
            }
            "empty" => {
                acl[1].privileges = uqa_core::catalog_sequence::SequencePrivileges::default();
                "ACL contains an empty grant path"
            }
            "public" => {
                acl[0].role = None;
                "PUBLIC cannot hold grant options"
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
fn retained_sequence_authority_validation_retires_scratch_at_every_pending_boundary() {
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
fn retained_sequence_authority_validation_copies_utf8_role_names_in_bounded_fragments() {
    let mut roles = roles();
    let value = Arc::new(security(&roles));
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let short_names = finish(
        &mut input(&value, &Arc::new(roles.clone()), &memory, &token, &token).unwrap(),
        1,
    )
    .unwrap();
    assert_eq!(memory.used(), 0);
    let mut owner = roles["owner"].clone();
    roles.remove("owner").unwrap();
    owner.name = "주인🦆".repeat(2048);
    roles.insert(owner.name.clone(), owner).unwrap();
    let roles = Arc::new(roles);
    let mut input = input(&value, &roles, &memory, &token, &token).unwrap();
    assert!(finish(&mut input, 1).unwrap() > short_names);
    value.validate(&roles).unwrap();
    assert_eq!(memory.used(), 0);
}

#[test]
fn sequence_authority_projection_retains_its_original_name_and_path_allowance() {
    let roles = roles();
    let value = security(&roles);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(value.resolve_with_control(&roles, &control).unwrap());
    });
    assert_eq!(
        usize::try_from(allocations.bytes_current).unwrap(),
        memory.used()
    );
    assert!(usize::try_from(allocations.bytes_max).unwrap() <= memory.peak());
    let output = output.unwrap();
    assert_eq!(output.role_owner, "owner");
    let entries = output.acl.as_ref().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].role.role_name(), Some("grantor"));
    assert_eq!(entries[0].grantor.as_deref(), Some("owner"));
    assert_eq!(entries[1].role.role_name(), Some("reader"));
    assert_eq!(entries[1].grantor.as_deref(), Some("grantor"));
    token.cancel();
    assert!(value.validate_with_control(&roles, &control).is_err());
    assert_eq!(output.role_owner, "owner");
    drop(output);
    assert_eq!(memory.used(), 0);
}
