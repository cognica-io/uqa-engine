//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{metadata_key, persistence, restore, SystemAcl};
use super::*;
use uqa_core::catalog_role::BoundAclEntry;
use uqa_sql::catalog::{
    roles::{RoleDefinition, RoleIdentity},
    security::{system_relations::SystemRelationSecurity, TablePrivileges},
};

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 1 << 20,
};
fn fixture(count: usize) -> (Arc<dyn CatalogFacade>, RoleDefinitions, String) {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = Arc::new(storage.catalog());
    let mut roles =
        RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap();
    let mut acl = Vec::new();
    for index in 0..count {
        let mut role = RoleDefinition::bootstrap();
        role.name = format!("reader_{index}");
        role.oid = 20000 + index as i64;
        role.object_id = [1; 16];
        role.attributes.clear();
        acl.push(BoundAclEntry {
            role: Some(role.identity()),
            grantor: RoleIdentity::BOOTSTRAP,
            privileges: TablePrivileges {
                select: true,
                ..TablePrivileges::default()
            },
            grant_options: TablePrivileges::default(),
        });
        roles.insert(role.name.clone(), role).unwrap();
    }
    let text = persistence::encode(&SystemAcl {
        revision: [1; 16],
        acl,
    })
    .unwrap();
    catalog
        .set_metadata(
            &metadata_key(
                SystemRelation::Projected(uqa_sql::catalog::VirtualRelation::PgShadow),
                Some("usename"),
            ),
            &text,
        )
        .unwrap();
    (catalog, roles, text)
}
fn finish(
    input: &mut SystemRelationCatalogInput,
    steps: usize,
) -> StorageBackendResult<(SystemRelationSecurities, usize)> {
    for polls in 1..1_000_000 {
        if let Some(value) = input.advance(steps)? {
            return Ok((value, polls));
        }
    }
    panic!("system authority did not progress")
}
fn compare(catalog: &Arc<dyn CatalogFacade>, roles: &RoleDefinitions) {
    let expected = restore(catalog.as_ref(), roles, false);
    let control = StorageReadControl::with_limit(8 << 20);
    let mut input = SystemRelationCatalogInput::new(
        Arc::clone(catalog),
        roles.clone(),
        LIMITS,
        control.memory(),
        &control,
    )
    .unwrap();
    let actual = finish(&mut input, 1).map(|(value, _)| value);
    match (actual, expected) {
        (Ok(actual), Ok(expected)) => assert_eq!(actual, expected),
        (Err(StorageBackendError::Serde(_)), Err(StorageBackendError::Serde(_))) => (),
        (Err(actual), Err(expected)) => assert_eq!(actual.to_string(), expected.to_string()),
        (actual, expected) => panic!("{actual:?}, {expected:?}"),
    }
    assert!(input.advance(1).is_err());
    assert_eq!(control.memory().used(), 0);
}
fn compare_text(catalog: &Arc<dyn CatalogFacade>, roles: &RoleDefinitions, text: &str) {
    catalog
        .set_metadata(
            &metadata_key(
                SystemRelation::Projected(uqa_sql::catalog::VirtualRelation::PgShadow),
                Some("usename"),
            ),
            text,
        )
        .unwrap();
    compare(catalog, roles);
}
fn wrapper(key: &str, text: &str) -> String {
    format!(
        "{{{}:{}}}",
        serde_json::to_string(key).unwrap(),
        serde_json::to_string(text).unwrap()
    )
}

#[test]
fn persistent_system_authority_matches_generic_fields_and_whole_family_error_order() {
    let (catalog, roles, text) = fixture(1);
    compare(&catalog, &roles);
    for (end, _) in text.char_indices() {
        compare_text(&catalog, &roles, &text[..end]);
    }
    for text in ["null", "[]", "{}", "{bad JSON", "0"] {
        compare_text(&catalog, &roles, text);
    }
    let original: serde_json::Value = serde_json::from_str(&text).unwrap();
    for pointer in [
        "/system_acl_format",
        "/entry",
        "/entry/revision",
        "/entry/acl",
        "/entry/acl/0/role",
        "/entry/acl/0/grantor",
        "/entry/acl/0/privileges",
    ] {
        for replacement in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!([]),
            serde_json::json!(999_999),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = replacement;
            compare_text(&catalog, &roles, &changed.to_string());
        }
    }
    compare_text(&catalog, &roles, &text);
    for suffix in ["bad key", "public.missing:", "pg_catalog.pg_shadow:missing"] {
        let key = format!("{METADATA_PREFIX}{suffix}");
        catalog.set_metadata(&key, &text).unwrap();
        compare(&catalog, &roles);
        catalog.delete_metadata(&key).unwrap();
    }
    let mut invalid = original.clone();
    invalid["entry"]["acl"][0]["grantor"]["object_id"] = serde_json::json!(vec![0; 16]);
    compare_text(&catalog, &roles, &invalid.to_string());
    let later = metadata_key(SystemRelation::PgTablespace, None);
    catalog.set_metadata(&later, "{}").unwrap();
    compare(&catalog, &roles);
    catalog.delete_metadata(&later).unwrap();
    let alias = format!("{METADATA_PREFIX}\"pg_catalog\".\"pg_shadow\":usename");
    catalog.set_metadata(&alias, &text).unwrap();
    compare_text(&catalog, &roles, &text);
    let empty = persistence::encode(&SystemAcl {
        revision: [2; 16],
        acl: vec![],
    })
    .unwrap();
    compare_text(&catalog, &roles, &empty);
}

#[test]
fn persistent_system_authority_preserves_scalar_representations_and_grant_failures() {
    let (catalog, roles, text) = fixture(1);
    let control = StorageReadControl::with_limit(1 << 20);
    let production = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    let keys = marker::protocol::keys(&production).unwrap();
    compare_text(&catalog, &roles, &wrapper(keys.raw, &text));
    let original: serde_json::Value = serde_json::from_str(&text).unwrap();
    for pointer in [
        "/system_acl_format",
        "/entry",
        "/entry/revision",
        "/entry/acl",
        "/entry/acl/0/role",
        "/entry/acl/0/privileges/select",
    ] {
        let value = original.pointer(pointer).unwrap().to_string();
        let key = serde_json::to_string(pointer.rsplit('/').next().unwrap()).unwrap();
        let changed = original.to_string().replacen(
            &format!("{key}:{value}"),
            &format!("{key}:{}", wrapper(keys.raw, &value)),
            1,
        );
        assert_ne!(changed, original.to_string());
        compare_text(&catalog, &roles, &changed);
    }
    for value in ["1", "1.0", "-0", "4294967296", " 1 ", "bad"] {
        compare_text(
            &catalog,
            &roles,
            &text.replacen(
                "\"system_acl_format\":1",
                &format!("\"system_acl_format\":{}", wrapper(keys.number, value)),
                1,
            ),
        );
    }
    compare_text(
        &catalog,
        &roles,
        &text.replacen(
            "\"entry\":",
            &format!("\"entry\":{},\"entry\":", wrapper(keys.raw, "bad")),
            1,
        ),
    );
    for defect in [
        "empty",
        "duplicate",
        "unrooted",
        "column",
        "option",
        "public",
    ] {
        let mut changed = original.clone();
        let entry = &mut changed["entry"]["acl"][0];
        match defect {
            "empty" => entry["privileges"]["select"] = false.into(),
            "unrooted" => entry["grantor"] = entry["role"].clone(),
            "column" => entry["privileges"]["delete"] = true.into(),
            "option" => entry["grant_options"]["update"] = true.into(),
            "public" => {
                entry["role"] = serde_json::Value::Null;
                entry["grant_options"]["select"] = true.into();
            }
            _ => (),
        }
        if defect == "duplicate" {
            let copy = entry.clone();
            changed["entry"]["acl"].as_array_mut().unwrap().push(copy);
        }
        compare_text(&catalog, &roles, &changed.to_string());
    }
}

#[test]
fn persistent_system_authority_bounds_role_and_grant_work_and_retains_later_mutations() {
    let (catalog, roles, _) = fixture(24);
    let expected = restore(catalog.as_ref(), &roles, false).unwrap();
    let mut windows = Vec::new();
    for steps in [1, 7, 64] {
        let control = StorageReadControl::with_limit(8 << 20);
        let mutations = MemoryBudget::new(1 << 20);
        let mut input = SystemRelationCatalogInput::new(
            Arc::clone(&catalog),
            roles.clone(),
            LIMITS,
            &mutations,
            &control,
        )
        .unwrap();
        let (mut value, polls) = finish(&mut input, steps).unwrap();
        windows.push(polls);
        assert_eq!(value, expected);
        let occupied = control
            .memory()
            .reserve(control.memory().available())
            .unwrap();
        control.cancellation().cancel();
        let before = value.clone();
        let relation = SystemRelation::Projected(uqa_sql::catalog::VirtualRelation::PgShadow);
        value
            .insert(
                uqa_core::RelationIdentity::new(relation.namespace(), relation.name()),
                SystemRelationSecurity::default(),
            )
            .unwrap();
        assert_eq!(before, expected);
        assert_ne!(value, before);
        drop((value, before, input));
        assert_eq!(control.memory().used(), occupied.bytes());
        drop(occupied);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(mutations.used(), 0);
    }
    assert!(windows[0] > windows[1] && windows[1] > windows[2]);
}

#[test]
fn persistent_system_authority_retires_partial_groups_and_pending_validation() {
    let (catalog, roles, _) = fixture(24);
    let before = catalog.metadata_with_prefix("").unwrap();
    for cut in [1, 32, 256] {
        for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(8 << 20);
            let mutations = MemoryBudget::new(1 << 20);
            let mut input = SystemRelationCatalogInput::new(
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
                        panic!("retire system authority");
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
