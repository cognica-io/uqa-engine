//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored point keys preserve NULL and namespace identity without scanning unrelated entries.

use super::*;
use crate::{ValueIndexEntry, ValueIndexKey};

#[test]
fn point_entries_distinguish_unbuilt_absent_and_stored_null() {
    let store = store();
    let backend = crate::key_value::KeyValueStorageBackend::new(store);
    let key = ValueIndexKey::Column("price".into());
    assert_eq!(
        backend.read_btree_index_entry("items", &key, 1).unwrap(),
        ValueIndexEntry::Unbuilt
    );
    backend.replace_btree_index("items", &key, &[]).unwrap();
    assert_eq!(
        backend.read_btree_index_entry("items", &key, 1).unwrap(),
        ValueIndexEntry::Absent
    );
    backend
        .apply_btree_index_write(
            "items",
            1,
            Some(&BTreeMap::from([(key.clone(), Value::Null)])),
        )
        .unwrap();
    assert_eq!(
        backend.read_btree_index_entry("items", &key, 1).unwrap(),
        ValueIndexEntry::Present(Value::Null)
    );
    backend
        .apply_btree_index_write(
            "items",
            1,
            Some(&BTreeMap::from([(key.clone(), Value::Int(2))])),
        )
        .unwrap();
    assert_eq!(
        backend.read_btree_index_entry("items", &key, 1).unwrap(),
        ValueIndexEntry::Present(Value::Int(2))
    );
    backend.clear_btree_indexes("items").unwrap();
    assert_eq!(
        backend.read_btree_index_entry("items", &key, 1).unwrap(),
        ValueIndexEntry::Absent
    );
    backend.drop_btree_index("items", &key).unwrap();
    assert_eq!(
        backend.read_btree_index_entry("items", &key, 1).unwrap(),
        ValueIndexEntry::Unbuilt
    );
}

#[test]
fn point_entries_keep_namespaces_and_do_not_decode_other_documents() {
    let store = store();
    let backend = crate::key_value::KeyValueStorageBackend::new(store.clone());
    let column = ValueIndexKey::Column("price".into());
    let named = ValueIndexKey::Index("price".into());
    backend
        .replace_btree_index("items", &column, &[(1, Value::Int(10))])
        .unwrap();
    let evaluated = Value::Row(vec![Value::Int(20)]);
    backend
        .replace_btree_index("items", &named, &[(1, evaluated.clone())])
        .unwrap();
    let unrelated = crate::key_value::index_keys::btree_entry_key("items", &named, 2).unwrap();
    store.put(&unrelated, &[0xff]).unwrap();
    assert_eq!(
        backend.read_btree_index_entry("items", &column, 1).unwrap(),
        ValueIndexEntry::Present(Value::Int(10))
    );
    assert_eq!(
        backend.read_btree_index_entry("items", &named, 1).unwrap(),
        ValueIndexEntry::Present(evaluated)
    );
    assert!(backend.read_btree_index_entry("items", &named, 2).is_err());
    assert!(backend.load_btree_index("items", &named).is_err());
    let definition = crate::key_value::index_keys::btree_index_key("items", &named).unwrap();
    store.put(&definition, b"invalid format").unwrap();
    assert!(backend.read_btree_index_entry("items", &named, 1).is_err());
}
