//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{roles::RoleDefinition, type_definitions::TypeDefinitions};
use uqa_core::{
    catalog_role::RoleIdentity,
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken, RelationIdentity,
};

fn roles() -> RoleDefinitions {
    RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap()
}
fn fixture(count: u8) -> TypeDefinitions<StoredShellType> {
    TypeDefinitions::try_from_iter((0..count).map(|index| {
        let value = StoredShellType {
            object_id: [index + 1; 16],
            oid: 20000 + u32::from(index),
            identity: RelationIdentity::new("public", format!("s_{index:03}")),
            owner: RoleIdentity::BOOTSTRAP,
            usage_acl: None,
        };
        (value.identity.qualified_name(), value)
    }))
    .unwrap()
}
fn input(
    registry: TypeDefinitions<StoredShellType>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<(), SQLError> {
    let original = original.clone();
    let retained = memory.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&retained, &original, input.cancellation);
        validate_shell_registry_from_input(&registry, &roles(), &control, &mut input).await
    })
    .unwrap()
}
fn finish(input: &mut ImageDecoder<(), SQLError>, steps: usize) -> Result<usize, SQLError> {
    for count in 1..65536 {
        if input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
            .is_some()
        {
            return Ok(count);
        }
    }
    panic!("shell validation stopped progressing")
}

#[test]
fn shell_registry_validation_preserves_ordered_identity_oid_name_and_owner_errors() {
    for (variant, expected) in [
        (0, "invalid or duplicate shell identity for `public.s_000`"),
        (1, "invalid or duplicate shell OID 0 for `public.s_000`"),
        (2, "inconsistent shell name for `public.s_000`"),
        (
            3,
            "shell `public.s_000` references missing role incarnation 99",
        ),
    ] {
        let mut value = fixture(1)["public.s_000"].clone();
        value.owner.oid = 99;
        match variant {
            0 => {
                value.object_id = [0; 16];
                value.oid = 0;
            }
            1 => {
                value.oid = 0;
                value.identity.schema.clear();
            }
            2 => value.identity.schema.clear(),
            _ => (),
        }
        let registry = TypeDefinitions::try_from_iter([("public.s_000".into(), value)]).unwrap();
        assert_eq!(
            validate_shell_registry(&registry, &roles()).unwrap_err(),
            expected
        );
        for steps in [1, 7, 64] {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = input(registry.clone(), &memory, &token, &token);
            let SQLError::Internal(message) = finish(&mut input, steps).unwrap_err() else {
                panic!("wrong shell error")
            };
            assert_eq!(message, expected);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn shell_registry_validation_preserves_native_generations_across_windows_and_terminal_failures() {
    let registry = fixture(64);
    let mut counts = Vec::new();
    for steps in [1, 7, 64] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = input(registry.clone(), &memory, &token, &token);
        counts.push(finish(&mut input, steps).unwrap());
        assert_eq!(memory.used(), 0);
        assert!(finish(&mut input, steps).is_err());
    }
    assert!(counts[0] > counts[1] && counts[1] > counts[2]);
    for cut in [1, 16, 64] {
        for terminal in ["original", "invoking", "memory", "zero", "drop", "unwind"] {
            let memory = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = input(registry.clone(), &memory, &original, &invoking);
            for _ in 0..cut {
                assert!(input
                    .advance(
                        &mut std::io::empty(),
                        ImageDecodeWindow { bytes: 1, steps: 1 }
                    )
                    .unwrap()
                    .is_none());
            }
            let occupied = if terminal == "memory" {
                Some(memory.reserve(memory.available()).unwrap())
            } else {
                None
            };
            match terminal {
                "original" => original.cancel(),
                "invoking" => invoking.cancel(),
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _owner = input;
                        panic!("retire shell validation");
                    }));
                    assert_eq!(memory.used(), 0);
                    continue;
                }
                _ => (),
            }
            if terminal != "drop" {
                assert!(finish(&mut input, usize::from(terminal != "zero")).is_err());
            }
            drop((input, occupied));
            assert_eq!(memory.used(), 0);
        }
    }
    validate_shell_registry(&registry, &roles()).unwrap();
}
