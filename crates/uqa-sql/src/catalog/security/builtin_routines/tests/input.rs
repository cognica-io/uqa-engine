//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{
    collections::BTreeSet,
    panic::{catch_unwind, AssertUnwindSafe},
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, OwnedMap, ProductionControl},
    CancellationToken,
};

fn source(count: usize) -> (BuiltinRoutineSecurity, RoleDefinitions) {
    let mut roles =
        RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap();
    let mut execute_acl = Vec::new();
    let mut grantor = RoleIdentity::BOOTSTRAP;
    for index in 0..count {
        let mut role = RoleDefinition::bootstrap();
        role.name = format!("role_{index:03}");
        role.oid = 20_000 + index as i64;
        role.object_id = [1; 16];
        role.attributes.clear();
        let identity = role.identity();
        roles.insert(role.name.clone(), role).unwrap();
        execute_acl.push(RoutineAclEntry {
            role: Some(identity),
            grantor,
            grant_option: true,
        });
        grantor = identity;
    }
    execute_acl.reverse();
    (
        BuiltinRoutineSecurity {
            revision: [7; 16],
            execute_acl,
        },
        roles,
    )
}

fn input(
    source: BuiltinRoutineSecurity,
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
        validate_from_input(&source, &roles, &control, &mut work).await
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
    panic!("builtin authority validation did not progress")
}

#[test]
fn builtin_authority_input_bounds_grant_closure_and_sorted_role_resolution() {
    let (source, roles) = source(32);
    let expected: BTreeSet<_> = roles.values().map(RoleDefinition::identity).collect();
    assert_eq!(
        object_acl::grant_option_roles(RoleIdentity::BOOTSTRAP, Some(&source.execute_acl)),
        expected
    );
    assert_eq!(
        object_acl::acl_roles(RoleIdentity::BOOTSTRAP, Some(&source.execute_acl)),
        expected
    );
    let token = CancellationToken::new();
    let mut polls = Vec::new();
    for steps in [1, 7, 64] {
        let memory = MemoryBudget::new(1 << 20);
        let mut input = input(source.clone(), roles.clone(), &memory, &token, &token);
        let initial = memory.used();
        let mut count = 0;
        loop {
            let before = memory.used();
            let result = input
                .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
                .unwrap();
            let node = OwnedMap::<(Option<RoleIdentity>, RoleIdentity), ()>::entry_bytes();
            assert!(memory.used().saturating_sub(before) <= (steps * 3 + 2) * node);
            count += 1;
            if result.is_some() {
                break;
            }
            assert!(count < 65536);
        }
        assert!(memory.peak() > initial);
        assert_eq!(memory.used(), 0);
        assert!(finish(&mut input, 1).is_err());
        polls.push(count);
    }
    assert!(polls[0] > 1024 && polls[0] > polls[1] && polls[1] > polls[2]);
}

#[test]
fn builtin_authority_input_preserves_tuple_path_and_role_failure_precedence() {
    for corruption in 0..8 {
        let (mut source, mut roles) = source(4);
        let expected = match corruption {
            0 => {
                source.revision = [0; 16];
                source.execute_acl[0].role = None;
                Some("invalid builtin routine ACL tuple identity")
            }
            1 => {
                source.execute_acl[0].role = None;
                roles.remove("uqa").unwrap();
                Some("invalid builtin routine authority: PUBLIC cannot retain a grant option")
            }
            2 => {
                source.execute_acl[0].grantor.object_id = [0; 16];
                roles.remove("uqa").unwrap();
                Some("invalid builtin routine authority: missing ACL endpoint incarnation")
            }
            3 => {
                source.execute_acl.push(source.execute_acl[0].clone());
                roles.remove("role_001").unwrap();
                Some("invalid builtin routine authority: duplicate ACL grant path")
            }
            4 => {
                source.execute_acl = vec![
                    RoutineAclEntry {
                        role: Some(roles["role_000"].identity()),
                        grantor: roles["role_001"].identity(),
                        grant_option: true,
                    },
                    RoutineAclEntry {
                        role: Some(roles["role_001"].identity()),
                        grantor: roles["role_000"].identity(),
                        grant_option: true,
                    },
                ];
                Some("invalid builtin routine authority: ACL grantor has no owner-rooted grant option")
            }
            5 => {
                roles.remove("role_001").unwrap();
                Some("builtin routine ACL references missing role incarnation")
            }
            6 => {
                source.execute_acl.clear();
                roles.remove("uqa").unwrap();
                Some("builtin routine ACL references missing role incarnation")
            }
            _ => None,
        };
        let diagnostic = |result: Result<(), SQLError>| {
            result.map_err(|error| match error {
                SQLError::Internal(message) => message,
                error => panic!("unexpected authority diagnostic: {error}"),
            })
        };
        let expected = expected.map_or(Ok(()), |message| Err(message.to_string()));
        assert_eq!(diagnostic(validate(&source, &roles)), expected);
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        assert_eq!(
            diagnostic(finish(&mut input(source, roles, &memory, &token, &token), 1).map(|_| ())),
            expected
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn builtin_authority_input_keeps_selected_incarnations_across_later_role_changes() {
    let (source, mut roles) = source(8);
    let selected = roles.clone();
    let mut role = roles["role_000"].clone();
    roles.remove("role_000").unwrap();
    role.name = "renamed".into();
    roles.insert(role.name.clone(), role).unwrap();
    validate(&source, &roles).unwrap();
    roles
        .edit("renamed", |role| {
            role.object_id = [9; 16];
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(validate(&source, &roles).is_err());
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    finish(&mut input(source, selected, &memory, &token, &token), 1).unwrap();
    assert_eq!(memory.used(), 0);
}

#[test]
fn builtin_authority_input_retires_denied_cancelled_and_abandoned_scratch() {
    let (source, roles) = source(48);
    for terminal in ["original", "invoking", "memory", "zero", "drop", "unwind"] {
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
        let occupied = (terminal == "memory").then(|| memory.reserve(memory.available()).unwrap());
        match terminal {
            "original" => original.cancel(),
            "invoking" => invoking.cancel(),
            "unwind" => {
                let _ = catch_unwind(AssertUnwindSafe(move || {
                    let _owner = input;
                    panic!("retire builtin authority validation");
                }));
                assert_eq!(memory.used(), sibling.bytes());
                continue;
            }
            _ => (),
        }
        if terminal != "drop" {
            let error = finish(&mut input, usize::from(terminal != "zero")).unwrap_err();
            match terminal {
                "original" | "invoking" => assert_eq!(error.sqlstate(), Some("57014")),
                "memory" => assert_eq!(error.sqlstate(), Some("53200")),
                _ => (),
            }
            assert!(finish(&mut input, 1).is_err());
        }
        drop((input, occupied));
        assert_eq!(memory.used(), sibling.bytes());
    }
}
