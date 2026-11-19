//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::TypeDefinitions;
use crate::{
    ast::ColumnType,
    catalog::{
        composite_type::{self, StoredComposite, StoredCompositeAttribute},
        range_type::{self, StoredMultirange, StoredRange},
        roles::{RoleDefinition, RoleDefinitions},
    },
    SQLError,
};
use uqa_core::{
    catalog_role::RoleIdentity,
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, RelationIdentity,
};

fn roles() -> RoleDefinitions {
    RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap()
}
fn range(index: u8) -> StoredRange {
    let oid = 20000 + u32::from(index) * 16;
    StoredRange {
        object_id: [index * 2 + 1; 16],
        oid,
        array_oid: oid + 1,
        array_name: format!("_r_{index}"),
        identity: RelationIdentity::new("public", format!("r_{index}")),
        owner: RoleIdentity::BOOTSTRAP,
        subtype: ColumnType::Integer,
        subtype_operator_class: 1978,
        collation_oid: 0,
        canonical_oid: None,
        subtype_difference_oid: None,
        multirange: StoredMultirange {
            object_id: [index * 2 + 2; 16],
            oid: oid + 2,
            array_oid: oid + 3,
            array_name: format!("_m_{index}"),
            identity: RelationIdentity::new("other", format!("m_{index}")),
        },
        usage_acl: None,
    }
}
fn composite(index: u8) -> StoredComposite {
    let oid = 30000 + u32::from(index) * 16;
    StoredComposite {
        object_id: [index + 1; 16],
        oid,
        array_oid: oid + 1,
        relation_oid: oid + 2,
        array_name: format!("_c_{index}"),
        identity: RelationIdentity::new("public", format!("c_{index}")),
        owner: RoleIdentity::BOOTSTRAP,
        attributes: (1..=16)
            .map(|number| StoredCompositeAttribute {
                name: format!("a_{number}"),
                ty: ColumnType::Integer,
                collation: None,
                collation_oid: None,
                number,
                dropped: number % 2 == 0,
            })
            .collect(),
        usage_acl: None,
    }
}
#[derive(Clone)]
enum Fixture {
    Ranges(TypeDefinitions<StoredRange>),
    Composites(TypeDefinitions<StoredComposite>),
}
impl Fixture {
    fn ordinary(&self) -> Result<(), String> {
        match self {
            Self::Ranges(registry) => range_type::validate_range_registry(registry, &roles()),
            Self::Composites(registry) => {
                composite_type::validate_composite_registry(registry, &roles())
            }
        }
    }
}
fn input(
    fixture: Fixture,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<(), SQLError> {
    let retained = memory.clone();
    let original = original.clone();
    ImageDecoder::new(memory, invoking, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&retained, &original, work.cancellation);
        match fixture {
            Fixture::Ranges(registry) => {
                range_type::validate_range_registry_from_input(
                    &registry,
                    &roles(),
                    &control,
                    &mut work,
                )
                .await
            }
            Fixture::Composites(registry) => {
                composite_type::validate_composite_registry_from_input(
                    &registry,
                    &roles(),
                    &control,
                    &mut work,
                )
                .await
            }
        }
    })
    .unwrap()
}
fn finish(input: &mut ImageDecoder<(), SQLError>, steps: usize) -> Result<usize, SQLError> {
    for polls in 1..1_000_000 {
        if input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
            .is_some()
        {
            return Ok(polls);
        }
    }
    panic!("structured type validation did not progress")
}
fn diagnostic(fixture: Fixture, expected: &str) {
    assert_eq!(fixture.ordinary().unwrap_err(), expected);
    for steps in [1, 7, 64] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = input(fixture.clone(), &memory, &token, &token);
        let SQLError::Internal(message) = finish(&mut input, steps).unwrap_err() else {
            panic!("wrong structured type diagnostic")
        };
        assert_eq!(message, expected);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn structured_type_validation_preserves_original_competing_predicate_order() {
    for (variant, expected) in [
        (0, "invalid or duplicate range identity for `public.r_0`"),
        (1, "invalid or duplicate range OID 0 for `public.r_0`"),
        (2, "inconsistent range name for `public.r_0`"),
        (3, "invalid or duplicate range type name for `public.r_0`"),
        (
            4,
            "range `public.r_0` references missing role incarnation 99",
        ),
        (5, "invalid range support identity for `public.r_0`"),
    ] {
        let mut value = range(0);
        value.subtype_operator_class = 0;
        if variant < 5 {
            value.owner.oid = 99;
        }
        match variant {
            0 => {
                value.object_id = [0; 16];
                value.oid = 0;
            }
            1 => {
                value.oid = 0;
                value.identity.name = "wrong".into();
            }
            2 => value.identity.name = "wrong".into(),
            3 => value.array_name = value.identity.name.clone(),
            _ => (),
        }
        diagnostic(
            Fixture::Ranges(
                TypeDefinitions::try_from_iter([("public.r_0".into(), value)]).unwrap(),
            ),
            expected,
        );
    }
    for (variant, expected) in [
        (
            0,
            "invalid or duplicate composite identity for `public.c_0`",
        ),
        (1, "invalid or duplicate composite OID 0 for `public.c_0`"),
        (2, "inconsistent composite name for `public.c_0`"),
        (3, "invalid composite attributes for `public.c_0`"),
        (
            4,
            "composite `public.c_0` references missing role incarnation 99",
        ),
    ] {
        let mut value = composite(0);
        value.owner.oid = 99;
        match variant {
            0 => {
                value.object_id = [0; 16];
                value.oid = 0;
            }
            1 => {
                value.oid = 0;
                value.identity.name = "wrong".into();
            }
            2 => {
                value.identity.name = "wrong".into();
                value.attributes[0].number = 0;
            }
            3 => value.attributes[1].number = value.attributes[0].number,
            _ => (),
        }
        diagnostic(
            Fixture::Composites(
                TypeDefinitions::try_from_iter([("public.c_0".into(), value)]).unwrap(),
            ),
            expected,
        );
    }
}

#[test]
fn structured_type_validation_resumes_native_roots_and_retires_every_terminal_owner() {
    let ranges = TypeDefinitions::try_from_iter((0..64).map(|i| {
        let v = range(i);
        (v.identity.qualified_name(), v)
    }))
    .unwrap();
    let composites = TypeDefinitions::try_from_iter((0..64).map(|i| {
        let v = composite(i);
        (v.identity.qualified_name(), v)
    }))
    .unwrap();
    for fixture in [Fixture::Ranges(ranges), Fixture::Composites(composites)] {
        let mut visits = Vec::new();
        for steps in [1, 7, 64] {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut input = input(fixture.clone(), &memory, &token, &token);
            visits.push(finish(&mut input, steps).unwrap());
            assert_eq!(memory.used(), 0);
            assert!(finish(&mut input, steps).is_err());
        }
        assert!(visits[0] > visits[1] && visits[1] > visits[2]);
        for cut in [1, 16, 128] {
            for terminal in ["original", "invoking", "memory", "zero", "drop", "unwind"] {
                let memory = MemoryBudget::new(1 << 20);
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let mut input = input(fixture.clone(), &memory, &original, &invoking);
                for _ in 0..cut {
                    assert!(input
                        .advance(
                            &mut std::io::empty(),
                            ImageDecodeWindow { bytes: 1, steps: 1 }
                        )
                        .unwrap()
                        .is_none());
                }
                let occupied =
                    (terminal == "memory").then(|| memory.reserve(memory.available()).unwrap());
                match terminal {
                    "original" => original.cancel(),
                    "invoking" => invoking.cancel(),
                    "unwind" => {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                            let _owner = input;
                            panic!("retire structured validation");
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
        fixture.ordinary().unwrap();
    }
}

#[test]
fn range_validation_yields_inside_original_nested_subtype_observations() {
    let mut value = range(0);
    for _ in 0..4096 {
        value.subtype = ColumnType::Array(Box::new(value.subtype));
    }
    let registry = TypeDefinitions::try_from_iter([("public.r_0".into(), value)]).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = input(Fixture::Ranges(registry), &memory, &token, &token);
    assert!(finish(&mut input, 1).unwrap() > 4096);
    assert_eq!(memory.used(), 0);
}
