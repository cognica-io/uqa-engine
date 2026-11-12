//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::builtin_routines::{encode, metadata_key, restore};
use uqa_sql::{
    ast::RoutineAclEntry,
    catalog::roles::{RoleDefinition, RoleIdentity},
};

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 1 << 20,
};

fn fixture(count: usize) -> (Arc<dyn CatalogFacade>, RoleDefinitions) {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = Arc::new(storage.catalog());
    let mut roles =
        RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap();
    let mut entries = Vec::new();
    let mut grantor = RoleIdentity::BOOTSTRAP;
    for index in 0..count {
        let mut role = RoleDefinition::bootstrap();
        role.name = format!("reader_{index}");
        role.oid = 20_000 + index as i64;
        role.object_id = [1; 16];
        role.attributes.clear();
        entries.push(RoutineAclEntry {
            role: Some(role.identity()),
            grantor,
            grant_option: true,
        });
        grantor = role.identity();
        roles.insert(role.name.clone(), role).unwrap();
    }
    entries.reverse();
    for oid in [870, 2108] {
        let entry = super::super::BuiltinRoutineSecurity {
            revision: [1; 16],
            execute_acl: entries.clone(),
        };
        catalog
            .set_metadata(&metadata_key(oid), &encode(&entry).unwrap())
            .unwrap();
    }
    (catalog, roles)
}

fn finish(
    input: &mut BuiltinRoutineCatalogInput,
    steps: usize,
) -> StorageBackendResult<(BuiltinRoutineSecurities, usize)> {
    for polls in 1..1_000_000 {
        if let Some(values) = input.advance(steps)? {
            return Ok((values, polls));
        }
    }
    panic!("builtin authority input did not progress")
}

#[test]
fn persistent_builtin_authority_preserves_complete_roots_and_later_mutations() {
    for count in [0, 1, 24] {
        let (catalog, roles) = fixture(count);
        let before = catalog.metadata_with_prefix("").unwrap();
        let expected = restore(catalog.as_ref(), &roles).unwrap();
        let mut windows = Vec::new();
        for steps in [1, 7, 64] {
            let control = StorageReadControl::with_limit(8 << 20);
            let mutations = MemoryBudget::new(1 << 20);
            let mut input = BuiltinRoutineCatalogInput::new(
                Arc::clone(&catalog),
                roles.clone(),
                LIMITS,
                &mutations,
                &control,
            )
            .unwrap();
            let (mut values, polls) = finish(&mut input, steps).unwrap();
            windows.push(polls);
            assert_eq!(values, expected);
            assert!(input.advance(1).is_err());
            let snapshot = values.clone();
            let occupied = control
                .memory()
                .reserve(control.memory().available())
                .unwrap();
            control.cancellation().cancel();
            values
                .insert(
                    870,
                    super::super::BuiltinRoutineSecurity {
                        revision: [2; 16],
                        execute_acl: Vec::new(),
                    },
                )
                .unwrap();
            assert_eq!(snapshot, expected);
            assert_eq!(values[&870].revision, [2; 16]);
            drop((input, values, snapshot));
            assert_eq!(control.memory().used(), occupied.bytes());
            drop(occupied);
            assert_eq!(control.memory().used(), 0);
            assert_eq!(mutations.used(), 0);
        }
        assert!(windows[0] > windows[1] && windows[1] >= windows[2]);
        assert_eq!(catalog.metadata_with_prefix("").unwrap(), before);
    }
}

#[test]
fn persistent_builtin_authority_keeps_physical_key_format_and_semantic_error_order() {
    for fault in ["key", "oid", "format", "tuple", "role"] {
        let (catalog, roles) = fixture(0);
        let key = metadata_key(870);
        let mut value: serde_json::Value =
            serde_json::from_str(&catalog.get_metadata(&key).unwrap().unwrap()).unwrap();
        let key = match fault {
            "key" => format!("{METADATA_PREFIX}0870"),
            "oid" => metadata_key(u32::MAX),
            "format" => {
                value["builtin_routine_acl_format"] = 2.into();
                key
            }
            "tuple" => {
                value["entry"]["revision"] = serde_json::json!(vec![0; 16]);
                key
            }
            _ => {
                value["entry"]["execute_acl"] = serde_json::json!([{"role":{"oid":12345,"object_id":vec![1;16]},"grantor":RoleIdentity::BOOTSTRAP,"grant_option":false}]);
                key
            }
        };
        catalog.set_metadata(&key, &value.to_string()).unwrap();
        let expected = restore(catalog.as_ref(), &roles).unwrap_err().to_string();
        let control = StorageReadControl::with_limit(4 << 20);
        let failure = |limits| {
            let mut input = BuiltinRoutineCatalogInput::new(
                Arc::clone(&catalog),
                roles.clone(),
                limits,
                control.memory(),
                &control,
            )
            .unwrap();
            let error = finish(&mut input, 1).unwrap_err();
            assert!(input.advance(1).is_err());
            assert_eq!(control.memory().used(), 0);
            error.to_string()
        };
        assert_eq!(failure(LIMITS), expected, "{fault}");
        let later = format!("{METADATA_PREFIX}zzzz");
        catalog.set_metadata(&later, &"x".repeat(4096)).unwrap();
        assert_ne!(
            failure(CatalogInputLimits {
                maximum_value_bytes: 2048,
                ..LIMITS
            }),
            expected,
            "{fault}"
        );
        catalog.delete_metadata(&later).unwrap();
        assert_eq!(failure(LIMITS), expected, "{fault}");
    }
}

#[test]
fn persistent_builtin_authority_retires_pending_records_and_grant_scratch() {
    let (catalog, roles) = fixture(48);
    let before = catalog.metadata_with_prefix("").unwrap();
    for cut in [1, 16, 128, 512] {
        for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(8 << 20);
            let mutations = MemoryBudget::new(1 << 20);
            let mut input = BuiltinRoutineCatalogInput::new(
                Arc::clone(&catalog),
                roles.clone(),
                LIMITS,
                &mutations,
                &control,
            )
            .unwrap();
            for _ in 0..cut {
                assert!(input.advance(1).unwrap().is_none());
            }
            let occupied = (terminal == "memory").then(|| {
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap()
            });
            match terminal {
                "cancel" => control.cancellation().cancel(),
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _owner = input;
                        panic!("retire authority");
                    }));
                    assert_eq!(control.memory().used(), 0);
                    continue;
                }
                _ => (),
            }
            if terminal != "drop" {
                assert!(finish(&mut input, usize::from(terminal != "zero")).is_err());
                assert!(input.advance(1).is_err());
            }
            drop((input, occupied));
            assert_eq!(control.memory().used(), 0);
            assert_eq!(mutations.used(), 0);
        }
    }
    assert_eq!(catalog.metadata_with_prefix("").unwrap(), before);
}
