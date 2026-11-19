//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::catalog_role::{BoundAclEntry, RoleIdentity};

const PROTOCOL: ScalarProtocol = ScalarProtocol {
    number: "test number representation",
    raw: "test raw representation",
};
fn fixture() -> String {
    serde_json::to_string(&StoredSecurity {
        database_security_format: 1,
        security: BoundDatabaseSecurity {
            role_owner: RoleIdentity::BOOTSTRAP,
            acl: Some(vec![BoundAclEntry {
                role: None,
                grantor: RoleIdentity::BOOTSTRAP,
                privileges: DatabasePrivileges::ALL,
                grant_options: DatabasePrivileges::default(),
            }]),
        },
    })
    .unwrap()
}
fn equivalent(text: &str) {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let value: serde_json::Value = serde_json::from_str(text).unwrap();
    let expected = serde_json::from_value::<StoredSecurity<BoundDatabaseSecurity>>(value);
    let actual = StoredSecurity::from_stored_value_json(
        text,
        PROTOCOL,
        &ProductionControl::new(&memory, &token, &token),
    );
    assert_eq!(
        actual.is_ok(),
        expected.is_ok(),
        "{text}: {actual:?}, {expected:?}"
    );
    if let (Ok(actual), Ok(expected)) = (actual, expected) {
        assert_eq!(
            actual.database_security_format,
            expected.database_security_format
        );
        assert_eq!(actual.security, expected.security);
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_database_fields_preserve_projected_defaults_duplicates_and_positional_shapes() {
    let text = fixture();
    equivalent(&text);
    let original: serde_json::Value = serde_json::from_str(&text).unwrap();
    for pointer in [
        "",
        "/security",
        "/security/role_owner",
        "/security/acl/0",
        "/security/acl/0/privileges",
        "/security/acl/0/grant_options",
    ] {
        let keys: Vec<_> = original
            .pointer(pointer)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        for key in keys {
            let mut changed = original.clone();
            changed
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(&key);
            equivalent(&changed.to_string());
        }
        let mut changed = original.clone();
        changed.pointer_mut(pointer).unwrap()["ignored"] = serde_json::json!([1, "값", null]);
        equivalent(&changed.to_string());
    }
    for format in ["-0", "1.0", "1e0", "4294967295", "4294967296", "null"] {
        equivalent(&text.replacen(
            "\"database_security_format\":1",
            &format!("\"database_security_format\":{format}"),
            1,
        ));
    }
    equivalent(&text.replacen("\"role_owner\":", "\"role_owner\":null,\"role_owner\":", 1));
    equivalent(&text.replacen("\"role\":null", "\"role\":{},\"role\":null", 1));
    let mut positional = original.clone();
    positional["security"] = serde_json::json!([
        original["security"]["role_owner"],
        original["security"]["acl"]
    ]);
    equivalent(&positional.to_string());
    positional["security"].as_array_mut().unwrap().pop();
    equivalent(&positional.to_string());
    equivalent(&serde_json::json!([1, original["security"]]).to_string());
}

#[test]
fn stored_database_authority_adopts_native_buffers_and_retires_denied_fields() {
    let memory = MemoryBudget::new(1 << 20);
    let mutations = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let stored = StoredSecurity::from_stored_value_json(&fixture(), PROTOCOL, &control)
        .unwrap()
        .into_budgeted()
        .unwrap();
    let pointer = stored.security.acl.as_ref().unwrap().as_ptr();
    let (stored, lease) = stored.into_parts();
    let value = adopt(Budgeted::new(stored.security, lease), &mutations, &original).unwrap();
    assert_eq!(value.acl.as_ref().unwrap().as_ptr(), pointer);
    let occupied = memory.reserve(memory.available()).unwrap();
    original.cancel();
    let successor = value
        .successor(&BoundDatabaseSecurity::bootstrap())
        .unwrap();
    assert!(successor.acl.is_none());
    assert!(value.acl.is_some());
    drop((value, successor));
    assert_eq!(memory.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(memory.used(), 0);
    assert_eq!(mutations.used(), 0);
    for limit in [0, 1, 64] {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        assert!(StoredSecurity::from_stored_value_json(
            &fixture(),
            PROTOCOL,
            &ProductionControl::new(&memory, &token, &token)
        )
        .is_err());
        assert_eq!(memory.used(), 0);
    }
}
