//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    builtin_routines::{BuiltinRoutineSecurities, BuiltinRoutineSecurity},
    capture::relations,
    system_relations::{SystemAcl, SystemRelationSecurities, SystemRelationSecurity},
    table_binding::BoundTableAclEntry,
    TablePrivileges,
};
use crate::{
    ast::ObjectAclEntry,
    catalog::{named_definitions::StoredDefinition, roles::RoleIdentity},
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::{
    memory::{MemoryBudget, OwnedMap, ProductionControl},
    CancellationToken, RelationIdentity,
};

fn routine(index: u32) -> BuiltinRoutineSecurity {
    BuiltinRoutineSecurity {
        revision: u128::from(index + 1).to_be_bytes(),
        execute_acl: vec![ObjectAclEntry {
            role: None,
            grantor: RoleIdentity::BOOTSTRAP,
            grant_option: index.is_multiple_of(2),
        }],
    }
}

fn system(index: u32) -> SystemRelationSecurity {
    let acl = SystemAcl {
        revision: u128::from(index + 1).to_be_bytes(),
        acl: vec![BoundTableAclEntry {
            role: Some(RoleIdentity {
                oid: i64::from(index) + 20000,
                object_id: [7; 16],
            }),
            grantor: RoleIdentity::BOOTSTRAP,
            privileges: TablePrivileges {
                select: true,
                update: true,
                ..TablePrivileges::default()
            },
            grant_options: TablePrivileges {
                select: true,
                ..TablePrivileges::default()
            },
        }],
    };
    SystemRelationSecurity {
        table: (!index.is_multiple_of(3)).then(|| acl.clone()),
        columns: OwnedMap::from_iter([
            ("Exact.Column".into(), acl),
            (
                "empty".into(),
                SystemAcl {
                    revision: [9; 16],
                    acl: Vec::new(),
                },
            ),
        ]),
    }
}

fn identity(index: u32) -> RelationIdentity {
    RelationIdentity::new("pg_catalog", format!("object_{index:04}"))
}

#[test]
fn authority_roots_capture_original_acl_tuples_without_allocations_or_encoding_changes() {
    for count in [1, 64, 4096] {
        let expected_routines = (0..count)
            .map(|index| (20000 + index, routine(index)))
            .collect::<BTreeMap<_, _>>();
        let expected_system = (0..count)
            .map(|index| (identity(index), system(index)))
            .collect::<BTreeMap<_, _>>();
        let mut routines =
            BuiltinRoutineSecurities::try_from_iter(expected_routines.clone()).unwrap();
        let mut systems = SystemRelationSecurities::try_from_iter(expected_system.clone()).unwrap();
        assert!(routines.iter().eq(expected_routines.iter()));
        assert!(systems.iter().eq(expected_system.iter()));
        let budget = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut saved = None;
        let allocations = allocation_counter::measure(|| {
            saved = Some((
                routines.capture(&control).unwrap(),
                systems.capture(&control).unwrap(),
            ));
        });
        assert_eq!(allocations.count_total, 0);
        let exact = MemoryBudget::new(budget.used());
        let exact_control = ProductionControl::new(&exact, &token, &token);
        let independent = (
            routines.capture(&exact_control).unwrap(),
            systems.capture(&exact_control).unwrap(),
        );
        assert_eq!(exact.used(), budget.used());
        let saved = saved.unwrap();
        let legacy_routines = relations::builtin_routines(&routines, &control).unwrap();
        let legacy_system = relations::system_relations(&systems, &control).unwrap();
        assert_eq!(
            serde_json::to_vec(&*saved.0).unwrap(),
            serde_json::to_vec(&*legacy_routines).unwrap()
        );
        assert_eq!(
            serde_json::to_vec(&routines).unwrap(),
            serde_json::to_vec(&expected_routines).unwrap()
        );
        for ((name, value), (old_name, old)) in saved.1.iter().zip(legacy_system.iter()) {
            assert_eq!(name, old_name);
            assert_eq!(value.table, old.table);
            assert!(value
                .columns
                .iter()
                .eq(old.columns.iter().map(|(k, v)| (k, v))));
            let columns = value
                .columns
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(
                serde_json::to_vec(&value.columns).unwrap(),
                serde_json::to_vec(&columns).unwrap()
            );
        }
        routines.clear();
        systems.clear();
        drop((routines, systems, legacy_routines, legacy_system));
        assert!(saved.0.iter().eq(expected_routines.iter()));
        assert!(saved.1.iter().eq(expected_system.iter()));
        assert!(independent.0.iter().eq(expected_routines.iter()));
        assert!(independent.1.iter().eq(expected_system.iter()));
        drop((saved, independent));
        assert_eq!(budget.used(), 0);
        assert_eq!(exact.used(), 0);
    }
}

#[test]
fn selected_authority_publication_shares_unrelated_large_acl_payloads() {
    for count in [1, 32, 512] {
        let mut routines = BuiltinRoutineSecurities::try_from_iter(
            (0..count).map(|index| (20000 + index, routine(index))),
        )
        .unwrap();
        let mut systems = SystemRelationSecurities::try_from_iter(
            (0..count).map(|index| (identity(index), system(index))),
        )
        .unwrap();
        let mut large = routine(9000);
        large.execute_acl = vec![large.execute_acl[0].clone(); 8192];
        routines.insert(90000, large).unwrap();
        let mut large = system(9000);
        large.columns.insert(
            "large".repeat(32768),
            SystemAcl {
                revision: [8; 16],
                acl: vec![large.columns["Exact.Column"].acl[0].clone(); 4096],
            },
        );
        systems.insert(identity(9000), large).unwrap();
        let original = (routines.clone(), systems.clone());
        let allocations = allocation_counter::measure(|| {
            routines
                .edit(&20000, |value| {
                    value.revision = [6; 16];
                    value.execute_acl.clear();
                    Ok(())
                })
                .unwrap()
                .unwrap();
            systems
                .edit(&identity(0), |value| {
                    value.table = None;
                    value.columns.clear();
                    Ok(())
                })
                .unwrap()
                .unwrap();
        });
        assert!(
            allocations.bytes_total < 32768,
            "count={count}: {allocations:?}"
        );
        assert!(
            allocations.count_total < 256,
            "count={count}: {allocations:?}"
        );
        for key in original.0.keys().filter(|key| **key != 20000) {
            assert!(routines.shares_definition(key, &original.0));
        }
        for key in original.1.keys().filter(|key| **key != identity(0)) {
            assert!(systems.shares_definition(key, &original.1));
        }
        assert_eq!(original.0[&20000], routine(0));
        assert_eq!(original.1[&identity(0)], system(0));
        assert!(routines[&20000].execute_acl.is_empty());
        assert!(systems[&identity(0)].columns.is_empty());
    }
}

#[test]
fn authority_copy_and_failed_admission_preserve_complete_revisions_and_release_leases() {
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let value = system(1);
    let mut copied = None;
    let allocations =
        allocation_counter::measure(|| copied = Some(value.copy_definition(&control).unwrap()));
    let bytes = budget.used();
    assert_eq!(allocations.bytes_current, i64::try_from(bytes).unwrap());
    assert_eq!(&**copied.as_ref().unwrap(), &value);
    drop(copied);
    assert_eq!(budget.used(), 0);
    let too_small = MemoryBudget::new(bytes - 1);
    assert!(value
        .copy_definition(&ProductionControl::new(&too_small, &token, &token))
        .is_err());
    assert_eq!(too_small.used(), 0);
    let native = MemoryBudget::new(65536);
    let mut routines = BuiltinRoutineSecurities::with_budget(&native);
    let mut systems = SystemRelationSecurities::with_budget(&native);
    routines.insert(20000, routine(0)).unwrap();
    systems.insert(identity(0), system(0)).unwrap();
    let saved = (
        routines.capture(&control).unwrap(),
        systems.capture(&control).unwrap(),
    );
    let small = MemoryBudget::new(budget.used() - 1);
    let small_control = ProductionControl::new(&small, &token, &token);
    let first = routines.capture(&small_control).unwrap();
    assert!(systems.capture(&small_control).is_err());
    drop(first);
    assert_eq!(small.used(), 0);
    let blocked = native.reserve(native.available()).unwrap();
    assert!(routines
        .edit(&20000, |value| {
            value.execute_acl.clear();
            Ok(())
        })
        .is_err());
    assert!(systems
        .edit(&identity(0), |value| {
            value.columns.clear();
            Ok(())
        })
        .is_err());
    assert_eq!(routines[&20000], routine(0));
    assert_eq!(systems[&identity(0)], system(0));
    token.cancel();
    assert!(matches!(
        routines.capture(&control),
        Err(SQLError::Cancelled(_))
    ));
    assert!(matches!(
        systems.capture(&control),
        Err(SQLError::Cancelled(_))
    ));
    drop((blocked, routines, systems));
    assert!(native.used() > 0);
    drop(saved);
    assert_eq!(native.used(), 0);
    assert_eq!(budget.used(), 0);
}
