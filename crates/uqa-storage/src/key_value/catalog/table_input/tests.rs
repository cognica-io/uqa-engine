//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::key_value::{catalog::keys::relation_key, codec::single_str_key};
use crate::{RelationIdentity, RelationSecurityRow};

#[test]
fn key_value_table_input_preserves_original_key_identity_and_validation_order() {
    for relation in [
        RelationIdentity::new("public", "legacy"),
        RelationIdentity::new("Quoted.Schema", "Ta\"ble"),
    ] {
        let table = TableSchema {
            relation: relation.clone(),
            security: RelationSecurityRow::bootstrap(),
            object_id: [1; 16],
            storage_generation: [2; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: String::new(),
            constraints_json: String::new(),
        };
        let bytes = serde_json::to_vec(&table).unwrap();
        {
            let key = relation_key(TAG_TABLE, &relation).unwrap();
            let control = StorageReadControl::with_limit(65536);
            let actual = definition(&key, &bytes, &control).unwrap().unwrap();
            assert_eq!(actual.relation, relation);
            assert_eq!(actual.security, table.security);
            assert_eq!(
                serde_json::to_value(&*actual).unwrap(),
                serde_json::to_value(&table).unwrap()
            );
            drop(actual);
            assert_eq!(control.memory().used(), 0);
            let other = relation_key(TAG_TABLE, &RelationIdentity::new("public", "other")).unwrap();
            assert!(definition(&other, &bytes, &control)
                .unwrap_err()
                .to_string()
                .contains("disagrees with stored relation"));
            assert_eq!(control.memory().used(), 0);
        }
        let legacy = single_str_key(TAG_TABLE, &relation.qualified_name()).unwrap();
        let control = StorageReadControl::with_limit(65536);
        let expected = super::super::keys::decode_relation_key(&legacy)
            .unwrap_err()
            .to_string();
        assert_eq!(
            definition(&legacy, &bytes, &control)
                .unwrap_err()
                .to_string(),
            expected
        );
        assert_eq!(control.memory().used(), 0);
    }
    let control = StorageReadControl::with_limit(65536);
    let expected = super::super::keys::decode_relation_key(b"\x01")
        .unwrap_err()
        .to_string();
    assert_eq!(
        definition(b"\x01", b"invalid table", &control)
            .unwrap_err()
            .to_string(),
        expected
    );
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn key_value_acl_selection_preserves_original_string_prefix_and_suffix_handling() {
    let name =
        crate::catalog::relation_acl::key(&RelationIdentity::new("public", "items"), Some("field"));
    let tuple = crate::catalog::relation_acl::RelationAclTuple::new(Some(Vec::new())).unwrap();
    let value = serde_json::to_string(&tuple).unwrap();
    let control = StorageReadControl::with_limit(65536);
    for suffix in [b"".as_slice(), b"legacy suffix"] {
        let mut key = single_str_key(TAG_METADATA, &name).unwrap();
        key.extend_from_slice(suffix);
        assert!(select_acl(&key, &control).unwrap());
        let actual = acl(&key, value.as_bytes(), &control).unwrap().unwrap();
        assert_eq!(actual.relation, RelationIdentity::new("public", "items"));
        assert_eq!(actual.column.as_deref(), Some("field"));
        assert_eq!(actual.tuple, tuple);
        drop(actual);
        assert_eq!(control.memory().used(), 0);
        let mut ignored = single_str_key(TAG_METADATA, "unrelated").unwrap();
        ignored.extend_from_slice(suffix);
        assert!(!select_acl(&ignored, &control).unwrap());
    }
    assert!(select_acl(&[TAG_METADATA], &control).is_err());
    let mut key = single_str_key(TAG_METADATA, "invalid").unwrap();
    *key.last_mut().unwrap() = 255;
    assert!(select_acl(&key, &control).is_err());
    assert_eq!(control.memory().used(), 0);
}
