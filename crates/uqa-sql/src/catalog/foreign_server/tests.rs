//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn persisted_option_order_must_describe_each_stored_key_once() {
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let mut server = definition();
    server.options = BTreeMap::from([("a".into(), "1".into()), ("b".into(), "2".into())]);
    for (order, valid) in [
        (None, true),
        (Some(vec!["b", "a"]), true),
        (Some(vec!["a", "a"]), false),
        (Some(vec!["a"]), false),
        (Some(vec!["a", "c"]), false),
    ] {
        server.metadata.option_order =
            order.map(|names| names.into_iter().map(str::to_owned).collect());
        assert_eq!(
            validate_foreign_servers(&BTreeMap::from([("source".into(), server.clone())]), &roles)
                .is_ok(),
            valid
        );
    }
}

fn definition() -> ForeignServerDefinition {
    ForeignServerDefinition {
        name: "source".into(),
        fdw_type: "memory_fdw".into(),
        options: BTreeMap::new(),
        metadata: ForeignServerMetadata {
            option_order: None,
            wrapper_reference: None,
            oid: 20_000,
            object_id: [1; 16],
            owner: RoleIdentity::BOOTSTRAP,
            server_type: None,
            version: None,
        },
    }
}

#[test]
fn foreign_server_owner_follows_role_incarnation_through_rename() {
    let mut role = RoleDefinition::bootstrap();
    let servers = BTreeMap::from([("source".into(), definition())]);
    role.name = "renamed".into();
    let mut roles = BTreeMap::from([("renamed".into(), role)]);
    validate_foreign_servers(&servers, &roles).unwrap();
    roles.get_mut("renamed").unwrap().object_id = [9; 16];
    assert!(validate_foreign_servers(&servers, &roles).is_err());
}

#[test]
fn foreign_server_registry_requires_unique_addresses_and_matching_names() {
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    for duplicate_oid in [false, true] {
        let mut other = definition();
        other.name = "other".into();
        if duplicate_oid {
            other.metadata.object_id = [2; 16];
        } else {
            other.metadata.oid += 1;
        }
        let servers = BTreeMap::from([("source".into(), definition()), ("other".into(), other)]);
        assert!(validate_foreign_servers(&servers, &roles).is_err());
    }
    assert!(
        validate_foreign_servers(&BTreeMap::from([("wrong".into(), definition())]), &roles)
            .is_err()
    );
    assert!(validate_foreign_server_name("").is_err());
}
