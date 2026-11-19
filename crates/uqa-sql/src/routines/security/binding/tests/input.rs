//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, OwnedMap, ProductionControl},
    CancellationToken,
};

mod resources;

fn source(count: usize, name_bytes: usize) -> (CreateFunction, RoleDefinitions) {
    let original = roles();
    let mut definition = definition(&original);
    let owner = original["owner"].clone();
    let mut roles = RoleDefinitions::try_from_iter([("owner".into(), owner.clone())]).unwrap();
    let mut acl = Vec::new();
    let mut grantor = owner.identity();
    for index in 0..count {
        let mut role = owner.clone();
        role.name = format!("r{index:03}_{}", "x".repeat(name_bytes));
        role.oid = 30_000 + i64::try_from(index).unwrap();
        role.object_id = [19; 16];
        let identity = role.identity();
        roles.insert(role.name.clone(), role).unwrap();
        acl.push(RoutineAclEntry {
            role: Some(identity),
            grantor,
            grant_option: true,
        });
        grantor = identity;
    }
    // Reverse the rooted chain so closure needs repeated visits before ordered endpoint lookup.
    acl.reverse();
    definition.execute_acl = Some(acl);
    (definition, roles)
}

fn input(
    source: CreateFunction,
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
        validate_routine_authority_from_input(&source, &roles, &control, &mut work).await
    })
    .unwrap()
}

fn advance(input: &mut ImageDecoder<(), SQLError>, steps: usize) -> Result<Option<()>, SQLError> {
    input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
}

fn finish(input: &mut ImageDecoder<(), SQLError>, steps: usize) -> Result<usize, SQLError> {
    for polls in 1..65536 {
        if advance(input, steps)?.is_some() {
            return Ok(polls);
        }
    }
    panic!("routine authority input did not progress")
}

fn diagnostic(result: Result<(), SQLError>) -> Result<(), String> {
    result.map_err(|error| match error {
        SQLError::Internal(message) => message,
        error => panic!("unexpected routine authority error: {error}"),
    })
}

#[test]
fn routine_authority_input_preserves_original_owner_path_and_role_error_order() {
    for corruption in 0..13 {
        let (mut source, mut roles) = source(4, 0);
        let expected = match corruption {
            0 => {
                source.owner = None;
                source.execute_acl.as_mut().unwrap()[0].role = None;
                Some("routine `f` has no bound catalog owner")
            }
            1 => {
                source.owner.as_mut().unwrap().object_id = [0; 16];
                Some("routine `f` has no bound catalog owner")
            }
            2 => {
                source.execute_acl.as_mut().unwrap()[0].role = None;
                roles.remove("owner").unwrap();
                Some("invalid routine authority: PUBLIC cannot retain a grant option")
            }
            3 => {
                source.execute_acl.as_mut().unwrap()[0].grantor.object_id = [0; 16];
                roles.remove("owner").unwrap();
                Some("invalid routine authority: missing ACL endpoint incarnation")
            }
            4 => {
                let acl = source.execute_acl.as_mut().unwrap();
                acl.push(acl[0].clone());
                roles.remove("r001_").unwrap();
                Some("invalid routine authority: duplicate ACL grant path")
            }
            5 => {
                let acl = source.execute_acl.as_mut().unwrap();
                acl[3].grantor = acl[0].role.unwrap();
                Some("invalid routine authority: ACL grantor has no owner-rooted grant option")
            }
            6 => {
                roles.remove("r001_").unwrap();
                Some("invalid routine authority: missing role incarnation")
            }
            7 => {
                roles
                    .edit("r001_", |role| {
                        role.object_id = [88; 16];
                        Ok(())
                    })
                    .unwrap()
                    .unwrap();
                Some("invalid routine authority: missing role incarnation")
            }
            8 => {
                source.execute_acl = None;
                None
            }
            9 => {
                source.execute_acl = Some(Vec::new());
                None
            }
            10 => {
                source.execute_acl = None;
                roles.remove("owner").unwrap();
                Some("invalid routine authority: missing role incarnation")
            }
            11 => {
                let owner = source.owner.unwrap();
                source.execute_acl.as_mut().unwrap().push(RoutineAclEntry {
                    role: None,
                    grantor: owner,
                    grant_option: false,
                });
                None
            }
            _ => None,
        }
        .map_or(Ok(()), |message| Err(message.to_owned()));
        // The original dependency projection is an independent complete named-result oracle.
        assert_eq!(
            diagnostic(routine_role_dependencies(&source, &roles).map(|_| ())),
            expected
        );
        assert_eq!(
            diagnostic(validate_routine_authority(&source, &roles)),
            expected
        );
        for steps in [1, 7, 64] {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = input(source.clone(), roles.clone(), &memory, &token, &token);
            assert_eq!(diagnostic(finish(&mut input, steps).map(|_| ())), expected);
            assert_eq!(memory.used(), 0);
            assert!(advance(&mut input, 1).is_err());
        }
    }
}

#[test]
fn routine_authority_input_bounds_grant_and_role_work_without_copying_names() {
    let (source, roles) = source(32, 0);
    let token = CancellationToken::new();
    let mut polls = Vec::new();
    for steps in [1, 7, 64] {
        let memory = MemoryBudget::new(1 << 20);
        let mut input = input(source.clone(), roles.clone(), &memory, &token, &token);
        let initial = memory.used();
        let mut count = 0;
        loop {
            let before = memory.used();
            let complete = advance(&mut input, steps).unwrap().is_some();
            let node = OwnedMap::<(Option<RoleIdentity>, RoleIdentity), ()>::entry_bytes();
            assert!(memory.used().saturating_sub(before) <= (steps * 3 + 2) * node);
            count += 1;
            if complete {
                break;
            }
            assert!(count < 65536);
        }
        assert!(memory.peak() > initial);
        assert_eq!(memory.used(), 0);
        polls.push(count);
    }
    assert!(polls[0] > 1024 && polls[0] > polls[1] && polls[1] > polls[2]);

    let mut allocations = Vec::new();
    let mut dependency_allocations = Vec::new();
    for width in [0, 8192] {
        let (source, roles) = self::source(8, width);
        let before = serde_json::to_value(&source).unwrap();
        allocations.push(
            allocation_counter::measure(|| validate_routine_authority(&source, &roles).unwrap())
                .bytes_total,
        );
        dependency_allocations.push(
            allocation_counter::measure(|| {
                routine_role_dependencies(&source, &roles).unwrap();
            })
            .bytes_total,
        );
        assert_eq!(serde_json::to_value(&source).unwrap(), before);
        let memory = MemoryBudget::new(1 << 20);
        let mut input = input(source, roles, &memory, &token, &token);
        allocations.push(
            allocation_counter::measure(|| {
                finish(&mut input, 1).unwrap();
            })
            .bytes_total,
        );
        assert_eq!(memory.used(), 0);
    }
    assert_eq!(allocations[0], allocations[2]);
    assert_eq!(allocations[1], allocations[3]);
    assert!(dependency_allocations[1] >= dependency_allocations[0] + 8 * 8192);
}

#[test]
fn routine_authority_input_keeps_selected_roles_across_later_renames_and_replacements() {
    let (source, mut roles) = source(8, 0);
    let selected = roles.clone();
    let mut original = roles["r001_"].clone();
    roles.remove("r001_").unwrap();
    original.name = "renamed".into();
    roles.insert(original.name.clone(), original).unwrap();
    validate_routine_authority(&source, &roles).unwrap();
    roles
        .edit("renamed", |role| {
            role.object_id = [99; 16];
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(validate_routine_authority(&source, &roles).is_err());
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    finish(&mut input(source, selected, &memory, &token, &token), 1).unwrap();
    assert_eq!(memory.used(), 0);
}
