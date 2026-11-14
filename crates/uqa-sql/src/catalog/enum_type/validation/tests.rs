//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    enum_type::initial_enum_labels, roles::RoleDefinition, type_definitions::TypeDefinitions,
};
use uqa_core::{
    catalog_role::RoleIdentity,
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken, RelationIdentity,
};

fn roles() -> RoleDefinitions {
    RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap()
}

fn definition(index: u8) -> StoredEnum {
    let oid = 20000 + u32::from(index) * 8;
    StoredEnum {
        object_id: [index + 1; 16],
        oid,
        array_oid: oid + 1,
        array_name: format!("_e_{index:03}"),
        identity: RelationIdentity::new("public", format!("e_{index:03}")),
        owner: RoleIdentity::BOOTSTRAP,
        labels: initial_enum_labels(oid, &["a".into(), "z".into()], &[oid + 2, oid + 3]).unwrap(),
        usage_acl: None,
    }
}

fn fixture(count: u8) -> TypeDefinitions<StoredEnum> {
    TypeDefinitions::try_from_iter((0..count).map(|index| {
        let value = definition(index);
        (value.identity.qualified_name(), value)
    }))
    .unwrap()
}

fn input(
    registry: TypeDefinitions<StoredEnum>,
    roles: RoleDefinitions,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<(), SQLError> {
    let original = original.clone();
    let retained = memory.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&retained, &original, input.cancellation);
        validate_enum_registry_from_input(&registry, &roles, &control, &mut input).await
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
    panic!("enum validation stopped progressing")
}

#[test]
fn enum_registry_validation_preserves_ordered_diagnostics_before_owner_resolution() {
    for (variant, expected) in [
        (0, "invalid or duplicate enum identity for `public.e_000`"),
        (1, "invalid or duplicate enum OID 0 for `public.e_000`"),
        (2, "inconsistent enum name for `public.e_000`"),
        (3, "invalid enum label for `public.e_000`"),
        (4, "enum labels for `public.e_000` are out of order"),
        (
            5,
            "enum `public.e_000` references missing role incarnation 99",
        ),
    ] {
        let mut value = definition(0);
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
            2 => {
                value.identity.schema.clear();
                value.labels[0].label = "x".repeat(64);
            }
            3 => {
                value.labels[0].label = "x".repeat(64);
                value.labels[0].sort_order = 99.0;
            }
            4 => value.labels[0].sort_order = 99.0,
            _ => (),
        }
        let registry = TypeDefinitions::try_from_iter([("public.e_000".into(), value)]).unwrap();
        assert_eq!(
            validate_enum_registry(&registry, &roles()).unwrap_err(),
            expected
        );
        for window in [1, 7, 64] {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = input(registry.clone(), roles(), &memory, &token, &token);
            let SQLError::Internal(message) = finish(&mut input, window).unwrap_err() else {
                panic!("wrong validation error")
            };
            assert_eq!(message, expected);
            assert_eq!(memory.used(), 0);
        }
    }
    for duplicate_identity in [true, false] {
        let mut second = definition(1);
        let first = definition(0);
        if duplicate_identity {
            second.object_id = first.object_id;
        } else {
            second.labels[1].oid = first.array_oid;
        }
        let registry = TypeDefinitions::try_from_iter([
            (first.identity.qualified_name(), first),
            (second.identity.qualified_name(), second),
        ])
        .unwrap();
        let expected = if duplicate_identity {
            "invalid or duplicate enum identity for `public.e_001`"
        } else {
            "invalid or duplicate enum OID 20001 for `public.e_001`"
        };
        assert_eq!(
            validate_enum_registry(&registry, &roles()).unwrap_err(),
            expected
        );
    }
}

#[test]
fn enum_registry_validation_bounds_entry_label_and_role_work_and_releases_scratch() {
    for count in [0, 1, 64] {
        let registry = fixture(count);
        validate_enum_registry(&registry, &roles()).unwrap();
        let mut counts = Vec::new();
        for window in [1, 7, 64] {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = input(registry.clone(), roles(), &memory, &token, &token);
            counts.push(finish(&mut input, window).unwrap());
            assert_eq!(memory.used(), 0);
            assert!(finish(&mut input, window).is_err());
        }
        if count != 0 {
            assert!(counts[0] > counts[1] && counts[1] >= counts[2]);
        }
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        validate_enum_registry_with_control(
            &registry,
            &roles(),
            &ProductionControl::new(&memory, &token, &token),
        )
        .unwrap();
        let peak = memory.peak();
        assert_eq!(memory.used(), 0);
        if peak != 0 {
            let small = MemoryBudget::new(peak - 1);
            assert_eq!(
                validate_enum_registry_with_control(
                    &registry,
                    &roles(),
                    &ProductionControl::new(&small, &token, &token)
                )
                .unwrap_err()
                .sqlstate(),
                Some("53200")
            );
            assert_eq!(small.used(), 0);
        }
    }
}

#[test]
fn enum_registry_validation_retires_native_scratch_on_every_terminal_boundary() {
    let registry = fixture(64);
    for cut in [1, 16, 128] {
        for terminal in ["original", "invoking", "memory", "zero", "drop", "unwind"] {
            let memory = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = input(registry.clone(), roles(), &memory, &original, &invoking);
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
                        panic!("retire enum validation");
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
    validate_enum_registry(&registry, &roles()).unwrap();
}
