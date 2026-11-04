//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::{table_binding::BoundTableAclEntry, TablePrivileges};
use uqa_core::{catalog_role::RoleIdentity, memory::MemoryBudget, CancellationToken};

fn entry() -> BoundTableAclEntry {
    BoundTableAclEntry {
        role: Some(RoleIdentity {
            oid: 17_001,
            object_id: [7; 16],
        }),
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: TablePrivileges {
            select: true,
            ..TablePrivileges::default()
        },
        grant_options: TablePrivileges {
            select: true,
            ..TablePrivileges::default()
        },
    }
}

#[test]
fn captured_system_authority_keeps_absent_empty_and_tombstone_acl_identities() {
    let memory = MemoryBudget::new(16 << 10);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    for table in [
        None,
        Some(SystemAcl {
            revision: [2; 16],
            acl: vec![],
        }),
        Some(SystemAcl {
            revision: [3; 16],
            acl: vec![entry()],
        }),
    ] {
        let source = SystemRelationSecurities::try_from_iter([(
            RelationIdentity::new("pg_catalog", "pg_class"),
            SystemRelationSecurity {
                table,
                columns: uqa_core::memory::OwnedMap::from_iter([
                    (
                        "empty".into(),
                        SystemAcl {
                            revision: [4; 16],
                            acl: vec![],
                        },
                    ),
                    (
                        "Mixed.Column".into(),
                        SystemAcl {
                            revision: [5; 16],
                            acl: vec![entry()],
                        },
                    ),
                ]),
            },
        )])
        .unwrap();
        let captured = system_relations(&source, &control)
            .unwrap()
            .into_budgeted()
            .unwrap();
        let (name, value) = &captured[0];
        assert_eq!(name, source.keys().next().unwrap());
        assert_eq!(value.table, source.values().next().unwrap().table);
        assert!(value.columns.iter().map(|(k, v)| (k, v)).eq(source
            .values()
            .next()
            .unwrap()
            .columns
            .iter()));
        drop(source);
        assert_eq!(value.columns[1].1.revision, [4; 16]);
        assert!(value.columns[1].1.acl.is_empty());
        assert!(memory.used() > 0);
        drop(captured);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn captured_routine_and_foreign_authority_preserve_complete_tuple_and_bound_identity() {
    let memory = MemoryBudget::new(16 << 10);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let routines = BuiltinRoutineSecurities::try_from_iter([
        (
            42,
            BuiltinRoutineSecurity {
                revision: [8; 16],
                execute_acl: vec![ObjectAclEntry {
                    role: None,
                    grantor: RoleIdentity::BOOTSTRAP,
                    grant_option: false,
                }],
            },
        ),
        (
            43,
            BuiltinRoutineSecurity {
                revision: [9; 16],
                execute_acl: vec![ObjectAclEntry {
                    role: entry().role,
                    grantor: RoleIdentity::BOOTSTRAP,
                    grant_option: true,
                }],
            },
        ),
    ])
    .unwrap();
    let mut security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    security.acl = Some(vec![entry()]);
    security
        .column_acls
        .insert("Exact.Column".into(), vec![entry()]);
    security.acl_revisions.relation = Some([10; 16]);
    security
        .acl_revisions
        .columns
        .insert("Exact.Column".into(), [11; 16]);
    security
        .acl_revisions
        .columns
        .insert("removed".into(), [12; 16]);
    let source = BTreeMap::from([(
        RelationIdentity::new("Quoted.Schema", "Foreign.Name"),
        security,
    )]);
    let saved_routines = builtin_routines(&routines, &control)
        .unwrap()
        .into_budgeted()
        .unwrap();
    let saved_tables = tables(&source, &control).unwrap().into_budgeted().unwrap();
    assert!(saved_routines
        .iter()
        .map(|(k, v)| (k, v))
        .eq(routines.iter()));
    drop((routines, source));
    assert_eq!(
        saved_tables[0].0,
        RelationIdentity::new("Quoted.Schema", "Foreign.Name")
    );
    let saved = &saved_tables[0].1;
    assert_eq!(saved.privileges.role_owner, RoleIdentity::BOOTSTRAP);
    assert_eq!(saved.privileges.acl(), Some([entry()].as_slice()));
    assert_eq!(
        saved.privileges.column_acls(),
        &[("Exact.Column".into(), vec![entry()])]
    );
    assert_eq!(saved.relation_revision, Some([10; 16]));
    assert_eq!(
        saved.column_revisions,
        [
            ("Exact.Column".into(), [11; 16]),
            ("removed".into(), [12; 16])
        ]
    );
    drop((saved_routines, saved_tables));
    assert_eq!(memory.used(), 0);
}

#[test]
fn relation_authority_capture_rejects_denied_payload_and_original_cancellation() {
    let memory = MemoryBudget::new(512);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let routine = BuiltinRoutineSecurity {
        revision: [1; 16],
        execute_acl: vec![
            ObjectAclEntry {
                role: None,
                grantor: RoleIdentity::BOOTSTRAP,
                grant_option: false
            };
            32_768
        ],
    };
    let system = SystemRelationSecurity {
        table: None,
        columns: uqa_core::memory::OwnedMap::from_iter([
            (
                "small".into(),
                SystemAcl {
                    revision: [2; 16],
                    acl: vec![entry()],
                },
            ),
            (
                "z".repeat(32_768),
                SystemAcl {
                    revision: [3; 16],
                    acl: vec![],
                },
            ),
        ]),
    };
    let foreign = BTreeMap::from([(
        RelationIdentity::new("s", "n".repeat(32_768)),
        BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP),
    )]);
    assert_eq!(
        routine.capture_security(&control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    assert_eq!(
        system.capture_security(&control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    assert_eq!(
        tables(&foreign, &control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    cancellation.cancel();
    assert!(matches!(
        routine.capture_security(&control),
        Err(SQLError::Cancelled(_))
    ));
    assert!(matches!(
        system.capture_security(&control),
        Err(SQLError::Cancelled(_))
    ));
    assert!(matches!(
        tables(&foreign, &control),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}
