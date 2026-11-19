//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::persistence;
use super::*;
use uqa_core::catalog_role::BoundAclEntry;
use uqa_sql::catalog::{
    roles::RoleDefinition,
    security::database::{BoundDatabaseSecurity, DatabasePrivileges},
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
    let mut authority = BoundDatabaseSecurity::bootstrap();
    authority.acl = Some(Vec::new());
    for index in 0..count {
        let mut role = RoleDefinition::bootstrap();
        role.name = format!("reader_{index}");
        role.oid = 20000 + index as i64;
        role.object_id = [1; 16];
        role.attributes.clear();
        authority.acl.as_mut().unwrap().push(BoundAclEntry {
            role: Some(role.identity()),
            grantor: authority.role_owner,
            privileges: DatabasePrivileges::ALL,
            grant_options: DatabasePrivileges::default(),
        });
        roles.insert(role.name.clone(), role).unwrap();
    }
    let json = persistence::encode(&authority, &roles).unwrap();
    catalog
        .set_metadata(DATABASE_SECURITY_METADATA_KEY, &json)
        .unwrap();
    (catalog, roles, json)
}
fn finish(
    input: &mut DatabaseCatalogInput,
    steps: usize,
) -> StorageBackendResult<(DatabaseAuthority, usize)> {
    for polls in 1..1_000_000 {
        if let Some(value) = input.advance(steps)? {
            return Ok((value, polls));
        }
    }
    panic!("database authority input did not progress")
}
fn compare(catalog: &Arc<dyn CatalogFacade>, roles: &RoleDefinitions, text: &str) {
    catalog
        .set_metadata(DATABASE_SECURITY_METADATA_KEY, text)
        .unwrap();
    let expected = persistence::restore(
        catalog.as_ref(),
        roles,
        false,
        &DatabaseAuthority::bootstrap(),
    );
    let control = StorageReadControl::with_limit(8 << 20);
    let mut input = DatabaseCatalogInput::new(
        Arc::clone(catalog),
        roles.clone(),
        LIMITS,
        control.memory(),
        &control,
    )
    .unwrap();
    let actual = finish(&mut input, 1).map(|(value, _)| value);
    match (actual, expected) {
        (Ok(actual), Ok(expected)) => assert_eq!(actual, expected, "{text}"),
        (Err(StorageBackendError::Serde(_)), Err(StorageBackendError::Serde(_))) => (),
        (Err(actual), Err(expected)) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{text}");
        }
        (actual, expected) => panic!("{text}: {actual:?}, {expected:?}"),
    }
    assert!(input.advance(1).is_err());
    assert_eq!(control.memory().used(), 0);
}
fn wrapper(key: &str, text: &str) -> String {
    format!(
        "{{{}:{}}}",
        serde_json::to_string(key).unwrap(),
        serde_json::to_string(text).unwrap()
    )
}

#[test]
fn persistent_database_authority_matches_generic_json_projection_and_error_order() {
    let (catalog, roles, text) = fixture(1);
    compare(&catalog, &roles, &text);
    for (end, _) in text.char_indices() {
        compare(&catalog, &roles, &text[..end]);
    }
    for json in ["null", "[]", "{}", "{bad JSON", "0"] {
        compare(&catalog, &roles, json);
    }
    let original: serde_json::Value = serde_json::from_str(&text).unwrap();
    for pointer in [
        "/database_security_format",
        "/security",
        "/security/role_owner",
        "/security/role_owner/oid",
        "/security/acl/0/role",
        "/security/acl/0/grantor",
        "/security/acl/0/privileges/connect",
    ] {
        for replacement in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!([]),
            serde_json::json!(999_999),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = replacement;
            compare(&catalog, &roles, &changed.to_string());
        }
    }
    compare(
        &catalog,
        &roles,
        &text.replacen("\"role_owner\":", "\"role_owner\":false,\"role_owner\":", 1),
    );
}

#[test]
fn persistent_database_authority_preserves_public_scalar_representations_and_discarded_values() {
    let (catalog, roles, text) = fixture(1);
    let original: serde_json::Value = serde_json::from_str(&text).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let production = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    let keys = marker::protocol::keys(&production).unwrap();
    compare(&catalog, &roles, &wrapper(keys.raw, &text));
    compare(
        &catalog,
        &roles,
        &wrapper(keys.raw, &wrapper(keys.raw, &text)),
    );
    for field in [
        "/database_security_format",
        "/security",
        "/security/role_owner",
        "/security/role_owner/object_id",
        "/security/acl",
        "/security/acl/0",
        "/security/acl/0/privileges/connect",
    ] {
        let value = original.pointer(field).unwrap().to_string();
        let replacement = wrapper(keys.raw, &format!(" {value} "));
        let key = field.rsplit('/').next().unwrap();
        let (from, to) = if key == "0" {
            (value.clone(), replacement)
        } else {
            let key = serde_json::to_string(key).unwrap();
            (format!("{key}:{value}"), format!("{key}:{replacement}"))
        };
        let changed = original.to_string().replacen(&from, &to, 1);
        assert_ne!(changed, original.to_string());
        compare(&catalog, &roles, &changed);
    }
    for number in ["1", "1.0", "-0", "4294967296", " 1 ", "bad"] {
        let replacement = wrapper(keys.number, number);
        let changed = text.replacen(
            "\"database_security_format\":1",
            &format!("\"database_security_format\":{replacement}"),
            1,
        );
        compare(&catalog, &roles, &changed);
    }
    let owner = original["security"]["role_owner"].to_string();
    for key in [keys.number, keys.raw] {
        let late = format!(
            "{{\"ordinary\":false,{}:\"not a scalar\",{}",
            serde_json::to_string(key).unwrap(),
            &owner[1..]
        );
        compare(
            &catalog,
            &roles,
            &original.to_string().replacen(&owner, &late, 1),
        );
    }
    for number in ["10", "10.0", "-0"] {
        let value = wrapper(keys.number, number);
        let changed = original
            .to_string()
            .replacen("\"oid\":10", &format!("\"oid\":{value}"), 1);
        compare(&catalog, &roles, &changed);
    }
    let invalid = wrapper(keys.raw, "bad");
    compare(
        &catalog,
        &roles,
        &text.replacen(
            "\"security\":",
            &format!("\"security\":{invalid},\"security\":"),
            1,
        ),
    );
    compare(
        &catalog,
        &roles,
        &format!("{{\"discarded\":{invalid},{}", &text[1..]),
    );
    let nested = format!("{}0{}", "[".repeat(128), "]".repeat(128));
    compare(
        &catalog,
        &roles,
        &format!("{{\"discarded\":{nested},{}", &text[1..]),
    );
}

#[test]
fn persistent_database_authority_retains_selected_roles_and_later_mutation_ownership() {
    let (catalog, roles, _) = fixture(32);
    let expected = persistence::restore(
        catalog.as_ref(),
        &roles,
        false,
        &DatabaseAuthority::bootstrap(),
    )
    .unwrap();
    let mut windows = Vec::new();
    for steps in [1, 7, 64] {
        let control = StorageReadControl::with_limit(8 << 20);
        let mutations = MemoryBudget::new(1 << 20);
        let mut input = DatabaseCatalogInput::new(
            Arc::clone(&catalog),
            roles.clone(),
            LIMITS,
            &mutations,
            &control,
        )
        .unwrap();
        let (value, polls) = finish(&mut input, steps).unwrap();
        windows.push(polls);
        assert_eq!(value, expected);
        let occupied = control
            .memory()
            .reserve(control.memory().available())
            .unwrap();
        control.cancellation().cancel();
        let next = value
            .successor(&BoundDatabaseSecurity::bootstrap())
            .unwrap();
        assert!(next.acl.is_none());
        assert_eq!(value, expected);
        drop((value, next, input));
        assert_eq!(control.memory().used(), occupied.bytes());
        drop(occupied);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(mutations.used(), 0);
    }
    assert!(windows[0] > windows[1] && windows[1] > windows[2]);
}

#[test]
fn persistent_database_authority_retires_partial_fields_and_pending_role_visits() {
    let (catalog, roles, _) = fixture(48);
    let before = catalog.metadata_with_prefix("").unwrap();
    for cut in [1, 32, 256] {
        for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(8 << 20);
            let mutations = MemoryBudget::new(1 << 20);
            let mut input = DatabaseCatalogInput::new(
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
                        panic!("retire database authority");
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
