//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_sql::catalog::roles::RoleIdentity;
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

fn roles() -> BTreeMap<String, RoleDefinition> {
    BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())])
}

#[test]
fn builtin_acl_round_trip_preserves_revoke_and_independent_tuple_revisions() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let lower = BuiltinRoutinePrivilegeUpdate::new(870, vec![]).unwrap();
    let aggregate = BuiltinRoutinePrivilegeUpdate::new(2108, vec![]).unwrap();
    for update in [&lower, &aggregate] {
        update.persist(Some(&catalog)).unwrap();
    }
    let expected = BTreeMap::from([(lower.oid, lower.entry), (aggregate.oid, aggregate.entry)]);
    let restored = restore(&catalog, &roles()).unwrap();
    assert_eq!(restored, expected);
    assert!(restored[&870].execute_acl.is_empty());
    let same = BuiltinRoutinePrivilegeUpdate::new(870, vec![]).unwrap();
    assert_ne!(same.entry.revision, restored[&870].revision);
    same.persist(Some(&catalog)).unwrap();
    let again = restore(&catalog, &roles()).unwrap();
    assert_eq!(again[&2108], restored[&2108]);
    assert_eq!(again[&870], same.entry);
}

#[test]
fn builtin_acl_restore_rejects_invalid_keys_versions_and_roles_without_writes() {
    let valid = BuiltinRoutinePrivilegeUpdate::new(
        870,
        vec![uqa_sql::ast::RoutineAclEntry {
            role: None,
            grantor: RoleIdentity::BOOTSTRAP,
            grant_option: false,
        }],
    )
    .unwrap();
    let baseline: serde_json::Value = serde_json::from_str(&encode(&valid.entry).unwrap()).unwrap();
    for fault in [
        "key",
        "unknown_oid",
        "version",
        "revision",
        "missing_grantee",
        "missing_grantor",
        "replacement",
    ] {
        let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
        let mut json = baseline.clone();
        let mut key = metadata_key(870);
        match fault {
            "key" => key = format!("{METADATA_PREFIX}0870"),
            "unknown_oid" => key = metadata_key(u32::MAX),
            "version" => json["builtin_routine_acl_format"] = 2.into(),
            "revision" => json["entry"]["revision"] = serde_json::json!(vec![0; 16]),
            "missing_grantee" => {
                json["entry"]["execute_acl"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("role");
            }
            "missing_grantor" => {
                json["entry"]["execute_acl"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("grantor");
            }
            _ => {
                json["entry"]["execute_acl"][0]["grantor"]["object_id"] =
                    serde_json::json!(vec![9; 16])
            }
        }
        let text = json.to_string();
        catalog.set_metadata(&key, &text).unwrap();
        assert!(restore(&catalog, &roles()).is_err(), "{fault}");
        assert_eq!(
            catalog.get_metadata(&key).unwrap().as_deref(),
            Some(text.as_str())
        );
    }
}
