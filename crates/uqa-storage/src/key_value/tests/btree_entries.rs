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
    let evaluated = Value::Row(vec![Value::Int(20)].into());
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

#[test]
fn stored_entry_visitor_bounds_wide_values_and_preserves_identity_order() {
    let backend = crate::key_value::KeyValueStorageBackend::new(store());
    let field = ValueIndexKey::Index("evaluated".into());
    let values: Vec<_> = (0..4096_u64)
        .map(|id| (id, Value::Str(format!("{id}:{}", "한".repeat(683)))))
        .chain([(u64::MAX, Value::Null)])
        .collect();
    backend
        .replace_btree_index("items", &field, &values)
        .unwrap();
    let mut count = 0;
    let allocation = allocation_counter::measure(|| {
        assert!(backend
            .visit_btree_index_entries("items", &field, &mut |id, value| {
                assert_eq!((id, &value), (values[count].0, &values[count].1));
                count += 1;
                Ok(())
            })
            .unwrap());
    });
    assert_eq!(count, values.len());
    assert!(allocation.bytes_max < 512 * 1024, "{allocation:?}");
    assert!(!backend
        .visit_btree_index_entries(
            "items",
            &ValueIndexKey::Column("evaluated".into()),
            &mut |_, _| panic!("unbuilt namespace"),
        )
        .unwrap());
    backend.clear_btree_indexes("items").unwrap();
    assert!(backend
        .visit_btree_index_entries("items", &field, &mut |_, _| panic!("empty index"))
        .unwrap());
}

#[test]
fn stored_entry_visitor_stops_before_later_corruption_and_preserves_errors() {
    let store = store();
    let backend = crate::key_value::KeyValueStorageBackend::new(store.clone());
    let field = ValueIndexKey::Column("key".into());
    backend
        .replace_btree_index("items", &field, &[(1, Value::Null), (2, Value::Int(2))])
        .unwrap();
    let key = crate::key_value::index_keys::btree_entry_key("items", &field, 2).unwrap();
    store.put(&key, &[0xff]).unwrap();
    let mut count = 0;
    let error = backend
        .visit_btree_index_entries("items", &field, &mut |id, value| {
            count += 1;
            assert_eq!((id, value), (1, Value::Null));
            Err(uqa_core::memory::MemoryError::Limit {
                required: 7,
                limit: 3,
            }
            .into())
        })
        .unwrap_err();
    assert!(matches!(
        error,
        StorageBackendError::Memory(uqa_core::memory::MemoryError::Limit {
            required: 7,
            limit: 3
        })
    ));
    assert_eq!(count, 1);
    assert!(backend
        .visit_btree_index_entries("items", &field, &mut |_, _| Ok(()))
        .is_err());
}
