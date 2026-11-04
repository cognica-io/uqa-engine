//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::RoleDefinition;
use crate::catalog::roles::RoleDefinitions;
use std::collections::BTreeMap;

#[test]
fn persisted_option_order_must_describe_each_stored_key_once() {
    let roles =
        RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap();
    let mut server = definition();
    server.options =
        uqa_core::memory::OwnedMap::from_iter([("a".into(), "1".into()), ("b".into(), "2".into())]);
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
        options: uqa_core::memory::OwnedMap::new(),
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
    let mut roles = RoleDefinitions::try_from_iter([("renamed".into(), role)]).unwrap();
    validate_foreign_servers(&servers, &roles).unwrap();
    roles
        .edit("renamed", |value| {
            value.object_id = [9; 16];
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(validate_foreign_servers(&servers, &roles).is_err());
}

#[test]
fn foreign_server_registry_requires_unique_addresses_and_matching_names() {
    let roles =
        RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap();
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
