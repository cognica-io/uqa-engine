//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::{RoleDefinition, RoleDefinitions};
use serde_json::{json, Value};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn stored_grant() -> Value {
    let owner = RoleIdentity::BOOTSTRAP;
    json!({"owner":owner,"namespace":2200,"object":b'r',"oid":16384,"acl":[{"role":null,"grantor":owner,"privileges":2,"grant_options":0}]})
}

#[test]
fn default_acl_metadata_rejects_invalid_grants_and_role_incarnations_before_publication() {
    let roles =
        RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut registry = DefaultAcls::with_budget(&memory);
    for (pointer, value) in [
        ("/oid", json!(0)),
        ("/object", json!(b'?')),
        ("/object", json!(b'n')),
        ("/acl/0/privileges", json!(0)),
        ("/acl/0/privileges", json!(128)),
        ("/acl/0/privileges", json!(65536)),
        ("/acl/0/grant_options", json!(2)),
        ("/acl/0/grant_options", json!(4)),
        ("/owner/object_id", json!(vec![1; 16])),
        ("/acl/0/grantor/oid", json!(42)),
        ("/acl", json!([])),
    ] {
        let mut record = stored_grant();
        *record.pointer_mut(pointer).unwrap() = value;
        assert!(
            registry
                .read_stored(&record.to_string(), &roles, &control)
                .is_err(),
            "{pointer}"
        );
        assert!(registry.is_empty());
        assert_eq!(memory.used(), 0);
    }
    let mut duplicate = stored_grant();
    let duplicate_entry = duplicate["acl"][0].clone();
    duplicate["acl"]
        .as_array_mut()
        .unwrap()
        .push(duplicate_entry);
    assert!(registry
        .read_stored(&duplicate.to_string(), &roles, &control)
        .is_err());
    let text = stored_grant().to_string();
    let key = registry.read_stored(&text, &roles, &control).unwrap();
    assert_eq!(registry.get(&key).unwrap().oid, 16_384);
    let retained = memory.used();
    assert!(registry.read_stored(&text, &roles, &control).is_err());
    assert_eq!(memory.used(), retained);
    drop(registry);
    assert_eq!(memory.used(), 0);
}
