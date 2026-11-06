//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::ObjectAclEntry,
    catalog::security::{
        builtin_routines::BuiltinRoutineSecurity,
        system_relations::{SystemAcl, SystemRelationSecurity},
    },
};
use uqa_core::{catalog_acl::TablePrivileges, memory::OwnedMap, RelationIdentity};

mod validation;

fn routine_fixture() -> BuiltinRoutineSecurities {
    let mut routines = BuiltinRoutineSecurities::new();
    let public = ObjectAclEntry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        grant_option: false,
    };
    let bound = ObjectAclEntry {
        role: Some(role("alpha", 42).identity()),
        grantor: role("omega", 99).identity(),
        grant_option: true,
    };
    routines
        .insert(
            870,
            BuiltinRoutineSecurity {
                revision: [7; 16],
                execute_acl: vec![public.clone(), bound, public],
            },
        )
        .unwrap();
    routines
        .insert(
            871,
            BuiltinRoutineSecurity {
                revision: [8; 16],
                execute_acl: vec![],
            },
        )
        .unwrap();
    routines
}

fn system_fixture() -> SystemRelationSecurities {
    let mut systems = SystemRelationSecurities::new();
    let public = BoundAclEntry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: TablePrivileges::ALL,
        grant_options: TablePrivileges {
            maintain: true,
            ..TablePrivileges::default()
        },
    };
    let bound = BoundAclEntry {
        role: Some(role("alpha", 42).identity()),
        grantor: role("omega", 99).identity(),
        privileges: TablePrivileges {
            select: true,
            update: true,
            ..TablePrivileges::default()
        },
        grant_options: TablePrivileges {
            select: true,
            ..TablePrivileges::default()
        },
    };
    systems
        .insert(
            RelationIdentity::new("pg_catalog", "pg_class"),
            SystemRelationSecurity {
                table: Some(SystemAcl {
                    revision: [1; 16],
                    acl: vec![public.clone(), bound.clone(), public],
                }),
                columns: [
                    (
                        "relname".into(),
                        SystemAcl {
                            revision: [2; 16],
                            acl: vec![bound.clone(), bound],
                        },
                    ),
                    (
                        "relnamespace".into(),
                        SystemAcl {
                            revision: [3; 16],
                            acl: vec![],
                        },
                    ),
                ]
                .into_iter()
                .collect(),
            },
        )
        .unwrap();
    for (name, table) in [
        ("pg_namespace", None),
        (
            "pg_type",
            Some(SystemAcl {
                revision: [4; 16],
                acl: vec![],
            }),
        ),
    ] {
        systems
            .insert(
                RelationIdentity::new("pg_catalog", name),
                SystemRelationSecurity {
                    table,
                    columns: OwnedMap::new(),
                },
            )
            .unwrap();
    }
    systems
}

fn images() -> [Vec<u8>; 2] {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    [
        streamed(
            routine_fixture()
                .capture(&control)
                .unwrap()
                .image_reader(&token)
                .unwrap(),
            7,
        ),
        streamed(
            system_fixture()
                .capture(&control)
                .unwrap()
                .image_reader(&token)
                .unwrap(),
            7,
        ),
    ]
}

#[test]
fn routine_authority_image_matches_literal_frame_and_retains_acl_paths() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut routines = BuiltinRoutineSecurities::new();
    routines
        .insert(
            870,
            BuiltinRoutineSecurity {
                revision: [7; 16],
                execute_acl: vec![],
            },
        )
        .unwrap();
    // Header, count, u32 OID, revision and an empty ACL vector are independent literal bytes.
    let mut expected = b"UQABRA01\x01\0\0\0\0\0\0\0\x66\x03\0\0".to_vec();
    expected.extend_from_slice(&[7; 16]);
    expected.extend_from_slice(&[0; 8]);
    let captured = routines.capture(&control).unwrap();
    assert_eq!(
        streamed(captured.image_reader(&token).unwrap(), 1),
        expected
    );
    let restored =
        BuiltinRoutineSecurities::read_image(&mut expected.as_slice(), &memory, &token).unwrap();
    assert!(restored == routines);
    routines
        .edit(&870, |value| {
            value.execute_acl.push(ObjectAclEntry {
                role: None,
                grantor: RoleIdentity::BOOTSTRAP,
                grant_option: true,
            });
            Ok(())
        })
        .unwrap();
    expected[36..44].copy_from_slice(&1_u64.to_le_bytes());
    expected.extend_from_slice(b"\0\x0a\0\0\0\0\0\0\0UQA:role00000010\x01");
    let literal = routines.capture(&control).unwrap();
    assert_eq!(streamed(literal.image_reader(&token).unwrap(), 1), expected);
    assert!(
        BuiltinRoutineSecurities::read_image(&mut expected.as_slice(), &memory, &token).unwrap()
            == routines
    );
    let routines = routine_fixture();
    let captured = routines.capture(&control).unwrap();
    let bytes = streamed(captured.image_reader(&token).unwrap(), 3);
    let restored =
        BuiltinRoutineSecurities::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert!(restored == routines);
    for width in [1, 2, 63, 4096, 20000] {
        assert_eq!(
            streamed(captured.image_reader(&token).unwrap(), width),
            bytes
        );
    }
}

fn table_privilege_masks() -> [TablePrivileges; 8] {
    [
        TablePrivileges {
            select: true,
            ..TablePrivileges::default()
        },
        TablePrivileges {
            insert: true,
            ..TablePrivileges::default()
        },
        TablePrivileges {
            update: true,
            ..TablePrivileges::default()
        },
        TablePrivileges {
            delete: true,
            ..TablePrivileges::default()
        },
        TablePrivileges {
            truncate: true,
            ..TablePrivileges::default()
        },
        TablePrivileges {
            references: true,
            ..TablePrivileges::default()
        },
        TablePrivileges {
            trigger: true,
            ..TablePrivileges::default()
        },
        TablePrivileges {
            maintain: true,
            ..TablePrivileges::default()
        },
    ]
}

#[test]
fn system_authority_image_matches_literal_frame_and_retains_column_revisions() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut systems = SystemRelationSecurities::new();
    systems
        .insert(
            RelationIdentity::new("pg_catalog", "pg_class"),
            SystemRelationSecurity::default(),
        )
        .unwrap();
    // Schema and local-name framing, zero columns and the absent table ACL are explicit.
    let expected = b"UQASRA01\x01\0\0\0\0\0\0\0\x0a\0\0\0\0\0\0\0pg_catalog\x08\0\0\0\0\0\0\0pg_class\0\0\0\0\0\0\0\0\0";
    let captured = systems.capture(&control).unwrap();
    assert_eq!(
        streamed(captured.image_reader(&token).unwrap(), 1),
        expected
    );
    let restored =
        SystemRelationSecurities::read_image(&mut expected.as_slice(), &memory, &token).unwrap();
    assert!(restored == systems);
    let masks = table_privilege_masks();
    for (bit, privileges) in masks.iter().enumerate() {
        systems
            .insert(
                RelationIdentity::new("pg_catalog", "pg_class"),
                SystemRelationSecurity {
                    table: Some(SystemAcl {
                        revision: [7; 16],
                        acl: vec![BoundAclEntry {
                            role: None,
                            grantor: RoleIdentity::BOOTSTRAP,
                            privileges: *privileges,
                            grant_options: masks[(bit + 1) % 8],
                        }],
                    }),
                    columns: OwnedMap::new(),
                },
            )
            .unwrap();
        let mut wire = expected.to_vec();
        *wire.last_mut().unwrap() = 1;
        wire.extend_from_slice(&[7; 16]);
        wire.extend_from_slice(b"\x01\0\0\0\0\0\0\0\0\x0a\0\0\0\0\0\0\0UQA:role00000010");
        wire.extend_from_slice(&[1 << bit, 1 << ((bit + 1) % 8)]);
        let literal = systems.capture(&control).unwrap();
        assert_eq!(streamed(literal.image_reader(&token).unwrap(), 1), wire);
        assert!(
            SystemRelationSecurities::read_image(&mut wire.as_slice(), &memory, &token).unwrap()
                == systems
        );
    }
    let systems = system_fixture();
    let captured = systems.capture(&control).unwrap();
    let bytes = streamed(captured.image_reader(&token).unwrap(), 3);
    let restored =
        SystemRelationSecurities::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert!(restored == systems);
    let mut written = vec![];
    assert_eq!(
        captured.write_image(&mut written, &token).unwrap(),
        bytes.len() as u64
    );
    assert_eq!(written, bytes);
    for width in [1, 2, 63, 4096, 20000] {
        assert_eq!(
            streamed(captured.image_reader(&token).unwrap(), width),
            bytes
        );
    }
}

#[test]
fn authority_image_readers_preserve_bounded_output_without_allocation() {
    let mut routines = routine_fixture();
    let mut systems = system_fixture();
    routines
        .edit(&870, |value| {
            value.execute_acl = value
                .execute_acl
                .iter()
                .cycle()
                .take(value.execute_acl.len() * 2000)
                .cloned()
                .collect();
            Ok(())
        })
        .unwrap();
    systems
        .edit(&RelationIdentity::new("pg_catalog", "pg_class"), |value| {
            for column in value.columns.values_mut() {
                column.acl = column
                    .acl
                    .iter()
                    .cycle()
                    .take(column.acl.len() * 2000)
                    .cloned()
                    .collect();
            }
            Ok(())
        })
        .unwrap();
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let routines = routines.capture(&control).unwrap();
    let systems = systems.capture(&control).unwrap();
    for saved in [
        &routines as &dyn AuthorityImage,
        &systems as &dyn AuthorityImage,
    ] {
        let expected = streamed(saved.reader(&token), 7);
        let mut output = vec![0; 20000];
        let allocations = allocation_counter::measure(|| {
            let mut reader = saved.reader(&token);
            let mut position = 0;
            loop {
                let count = reader.read(&mut output).unwrap();
                assert!(count <= 16384);
                assert_eq!(&output[..count], &expected[position..position + count]);
                position += count;
                if count == 0 {
                    break;
                }
            }
            assert_eq!(position, expected.len());
        });
        assert_eq!(allocations.count_total, 0);
    }
}

trait AuthorityImage {
    fn reader<'a>(&'a self, token: &'a CancellationToken) -> CatalogImageReader<'a>;
}
impl AuthorityImage for uqa_core::memory::Produced<CapturedBuiltinRoutineSecurities> {
    fn reader<'a>(&'a self, token: &'a CancellationToken) -> CatalogImageReader<'a> {
        self.image_reader(token).unwrap()
    }
}
impl AuthorityImage for uqa_core::memory::Produced<CapturedSystemRelationSecurities> {
    fn reader<'a>(&'a self, token: &'a CancellationToken) -> CatalogImageReader<'a> {
        self.image_reader(token).unwrap()
    }
}
