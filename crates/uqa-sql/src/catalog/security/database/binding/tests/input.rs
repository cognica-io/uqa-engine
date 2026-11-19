//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::SQLError;
use std::panic::{catch_unwind, AssertUnwindSafe};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn input(
    source: BoundDatabaseSecurity,
    roles: RoleDefinitions,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<(), SQLError> {
    let budget = memory.clone();
    let original = original.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&budget, &original, work.cancellation);
        source
            .validate_from_input(&roles, &control, &mut work)
            .await
    })
    .unwrap()
}

fn finish(input: &mut ImageDecoder<(), SQLError>, steps: usize) -> Result<usize, SQLError> {
    for polls in 1..65536 {
        if input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
            .is_some()
        {
            return Ok(polls);
        }
    }
    panic!("database authority validation did not progress")
}

fn source() -> (RoleDefinitions, BoundDatabaseSecurity) {
    let (mut roles, security) = fixture();
    let bound = BoundDatabaseSecurity::bind(&security, &roles).unwrap();
    for index in 0..64 {
        let role = role(&format!("a{index:02}"), 30_000 + index, 99);
        roles.insert(role.name.clone(), role).unwrap();
    }
    (roles, bound)
}

#[test]
fn database_authority_input_preserves_role_visits_without_copying_names_or_paths() {
    let (roles, source) = source();
    let original_acl = source.acl.as_ref().unwrap();
    let token = CancellationToken::new();
    let mut polls = Vec::new();
    for steps in [1, 7, 64] {
        let memory = MemoryBudget::new(1 << 20);
        let mut input = input(source.clone(), roles.clone(), &memory, &token, &token);
        let baseline = memory.used();
        let mut measured = None;
        let allocations = allocation_counter::measure(|| {
            measured = Some(finish(&mut input, steps).unwrap());
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(memory.peak(), baseline);
        assert_eq!(memory.used(), 0);
        assert!(finish(&mut input, 1).is_err());
        polls.push(measured.unwrap());
        assert_eq!(source.acl.as_ref().unwrap(), original_acl);
    }
    assert!(polls[0] > 64 && polls[0] > polls[1] && polls[1] > polls[2]);
}

#[test]
fn database_authority_input_preserves_owner_grantee_and_grantor_error_order() {
    let (roles, original) = source();
    for missing in [false, true] {
        for endpoint in 0..3 {
            let mut source = original.clone();
            let invalid = RoleIdentity {
                oid: if missing { 40_000 } else { 0 },
                object_id: [77; 16],
            };
            if endpoint == 0 {
                source.role_owner = invalid;
            }
            let acl = source.acl.as_mut().unwrap();
            if endpoint <= 1 {
                acl[0].role = Some(invalid);
            }
            acl[0].grantor = RoleIdentity {
                oid: 50_000,
                ..invalid
            };
            // Resolve is the unchanged ordinary owner/name projection, independent of the retained validator.
            let expected = source.resolve(&roles).unwrap_err();
            assert_eq!(source.validate(&roles).unwrap_err(), expected);
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = input(source, roles.clone(), &memory, &token, &token);
            let error = finish(&mut input, 1).unwrap_err();
            let SQLError::Internal(actual) = error else {
                panic!("unexpected authority diagnostic: {error}");
            };
            assert_eq!(actual, expected);
            assert_eq!(memory.used(), 0);
        }
    }
    for acl in [None, Some(Vec::new())] {
        let source = BoundDatabaseSecurity {
            acl,
            ..original.clone()
        };
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        finish(
            &mut input(source, roles.clone(), &memory, &token, &token),
            1,
        )
        .unwrap();
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn database_authority_input_retires_both_cancellation_and_abandoned_owners() {
    let (roles, source) = source();
    for terminal in ["original", "invoking", "zero", "drop", "unwind"] {
        let memory = MemoryBudget::new(1 << 20);
        let sibling = memory.reserve(17).unwrap();
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut input = input(source.clone(), roles.clone(), &memory, &original, &invoking);
        assert!(input
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow {
                    bytes: 1,
                    steps: 16
                }
            )
            .unwrap()
            .is_none());
        match terminal {
            "original" => original.cancel(),
            "invoking" => invoking.cancel(),
            "unwind" => {
                let _ = catch_unwind(AssertUnwindSafe(move || {
                    let _owner = input;
                    panic!("retire database authority validation");
                }));
                assert_eq!(memory.used(), sibling.bytes());
                continue;
            }
            _ => (),
        }
        if terminal != "drop" {
            let error = finish(&mut input, usize::from(terminal != "zero")).unwrap_err();
            if terminal != "zero" {
                assert_eq!(error.sqlstate(), Some("57014"));
            }
            assert!(finish(&mut input, 1).is_err());
        }
        drop(input);
        assert_eq!(memory.used(), sibling.bytes());
    }
}
