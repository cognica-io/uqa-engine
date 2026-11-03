//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::database::DatabasePrivileges;
use uqa_core::{
    catalog_role::RoleIdentity,
    catalog_schema::{SchemaPrivileges, SchemaTupleIdentity},
    catalog_sequence::SequencePrivileges,
    memory::MemoryBudget,
    CancellationToken,
};

fn entries<P: Copy + Default>(all: P) -> Vec<BoundAclEntry<P>> {
    vec![
        BoundAclEntry {
            role: None,
            grantor: RoleIdentity::BOOTSTRAP,
            privileges: all,
            grant_options: P::default(),
        },
        BoundAclEntry {
            role: Some(RoleIdentity {
                oid: 20001,
                object_id: [7; 16],
            }),
            grantor: RoleIdentity {
                oid: 20002,
                object_id: [8; 16],
            },
            privileges: all,
            grant_options: all,
        },
    ]
}

#[test]
fn captured_catalog_authority_preserves_acl_absence_grant_paths_and_namespace_tuple_identity() {
    let memory = MemoryBudget::new(8192);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    for mode in 0..3 {
        let database = BoundDatabaseSecurity {
            role_owner: RoleIdentity::BOOTSTRAP,
            acl: match mode {
                0 => None,
                1 => Some(vec![]),
                _ => Some(entries(DatabasePrivileges::ALL)),
            },
        };
        let sequence = BoundSequenceSecurity {
            role_owner: RoleIdentity::BOOTSTRAP,
            acl: match mode {
                0 => None,
                1 => Some(vec![]),
                _ => Some(entries(SequencePrivileges::ALL)),
            },
        };
        let schema = BoundSchemaSecurity {
            tuple: Some(SchemaTupleIdentity {
                oid: 17000,
                object_id: [3; 16],
                revision: [4; 16],
            }),
            role_owner: RoleIdentity::BOOTSTRAP,
            acl: match mode {
                0 => None,
                1 => Some(vec![]),
                _ => Some(entries(SchemaPrivileges::ALL)),
            },
        };
        let d = database
            .capture_security(&control)
            .unwrap()
            .into_budgeted()
            .unwrap();
        let q = sequence
            .capture_security(&control)
            .unwrap()
            .into_budgeted()
            .unwrap();
        let s = schema
            .capture_security(&control)
            .unwrap()
            .into_budgeted()
            .unwrap();
        assert_eq!(*d, database);
        assert_eq!(*q, sequence);
        assert_eq!(*s, schema);
        drop((database, sequence, schema, d, q, s));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn catalog_authority_capture_rejects_unfunded_or_cancelled_payload_without_partial_retention() {
    let mut database = BoundDatabaseSecurity::bootstrap();
    database.acl = Some(vec![entries(DatabasePrivileges::ALL)[0].clone(); 32768]);
    let memory = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    assert_eq!(
        database.capture_security(&control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    assert_eq!(database.acl.as_ref().unwrap().len(), 32768);
    let admitted = MemoryBudget::new(8 << 20);
    let control = ProductionControl::new(&admitted, &cancellation, &cancellation);
    let copied = database
        .capture_security(&control)
        .unwrap()
        .into_budgeted()
        .unwrap();
    drop(database);
    assert_eq!(copied.acl.as_ref().unwrap().len(), 32768);
    assert!(admitted.used() >= 32768 * size_of::<BoundAclEntry<DatabasePrivileges>>());
    drop(copied);
    assert_eq!(admitted.used(), 0);
    cancellation.cancel();
    assert!(matches!(
        BoundDatabaseSecurity::bootstrap().capture_security(&control),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(admitted.used(), 0);
}

#[test]
fn captured_authority_registries_preserve_exact_names_order_and_independent_payload_ownership() {
    let source = BTreeMap::from([
        (
            "Mixed.Name".into(),
            BoundSchemaSecurity::bootstrap("public"),
        ),
        (
            "이름".into(),
            BoundSchemaSecurity::with_public_privileges(true),
        ),
    ]);
    let sequences_source = BTreeMap::from([(
        RelationIdentity::new("Mixed.Name", "Quoted.Sequence"),
        BoundSequenceSecurity {
            role_owner: RoleIdentity::BOOTSTRAP,
            acl: Some(entries(SequencePrivileges::ALL)),
        },
    )]);
    let memory = MemoryBudget::new(8192);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let saved = schemas(&source, &control).unwrap().into_budgeted().unwrap();
    let saved_sequences = sequences(&sequences_source, &control)
        .unwrap()
        .into_budgeted()
        .unwrap();
    assert!(saved
        .iter()
        .map(|(key, value)| (key, value))
        .eq(source.iter()));
    assert_eq!(
        saved_sequences[0].0,
        RelationIdentity::new("Mixed.Name", "Quoted.Sequence")
    );
    assert_eq!(
        saved_sequences[0].1,
        sequences_source.values().next().unwrap().clone()
    );
    drop((source, sequences_source));
    assert_eq!(saved[0].0, "Mixed.Name");
    assert!(memory.used() > 0);
    drop((saved, saved_sequences));
    assert_eq!(memory.used(), 0);
}
