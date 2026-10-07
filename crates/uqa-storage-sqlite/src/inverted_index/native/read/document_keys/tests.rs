//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming native keys retain common byte order and fixed private visibility.

use super::*;
use crate::{catalog::Catalog, connection::ManagedConnection, inverted_index::SQLiteInvertedIndex};
use std::collections::BTreeMap;
use uqa_storage::{key_value::KeyValueRead, mvcc::VersionedSessionOptions, InvertedIndex};

#[test]
fn document_major_keys_preserve_order_continuations_and_private_views() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let _catalog = Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let mut index = SQLiteInvertedIndex::new(
        connection.clone(),
        "docs",
        uqa_analysis::whitespace_analyzer(),
    );
    connection.begin_transaction().unwrap();
    let fields = BTreeMap::from([
        ("a_long_name".into(), String::new()),
        ("z".into(), String::new()),
        ("é".into(), String::new()),
        ("a\0b".into(), String::new()),
    ]);
    for id in [0, 8, 256] {
        index.add_document(id, fields.clone()).unwrap();
    }
    connection.commit_transaction().unwrap();
    let committed = connection.native_snapshot().unwrap().unwrap();
    connection.begin_transaction().unwrap();
    index.remove_document(8).unwrap();
    index.add_document(16, fields).unwrap();
    let private = connection.native_snapshot().unwrap().unwrap();
    connection.rollback_transaction().unwrap();

    for (snapshot, ids) in [(&committed, [0, 8, 256]), (&private, [0, 16, 256])] {
        let read = NativeRead::new(snapshot, "docs").unwrap();
        for projection in [Projection::Length, Projection::Document] {
            let mut address = Address::table("docs");
            address.projection = Some(projection);
            let prefix = address.encode(read.control()).unwrap();
            let mut expected = Vec::new();
            for document in ids {
                for field in ["z", "é", "a\0b", "a_long_name"] {
                    let key = Address {
                        document: Some(document),
                        field: Some(field),
                        ..address
                    }
                    .encode(read.control())
                    .unwrap();
                    expected.push(key.to_vec());
                }
            }
            assert!(expected.windows(2).all(|pair| pair[0] < pair[1]));
            for after in [
                None,
                Some(&expected[0]),
                Some(&expected[5]),
                expected.last(),
            ] {
                for limit in [0, 1, 3, 5, usize::MAX] {
                    let mut actual = Vec::new();
                    read.visit_keys_after(
                        &prefix,
                        after.map(Vec::as_slice),
                        limit,
                        read.control(),
                        &mut |key| {
                            actual.push(key.to_vec());
                            Ok(())
                        },
                    )
                    .unwrap();
                    let wanted: Vec<_> = expected
                        .iter()
                        .filter(|key| after.is_none_or(|after| *key > after))
                        .take(limit)
                        .cloned()
                        .collect();
                    assert_eq!(actual, wanted);
                }
            }
            let control = StorageReadControl::with_limit(16 * 1024);
            let mut visited = 0;
            let result = read.visit_keys_after(&prefix, None, usize::MAX, &control, &mut |_| {
                visited += 1;
                control.cancellation().cancel();
                Ok(())
            });
            assert!(matches!(
                result,
                Err(uqa_storage::StorageBackendError::Cancelled(_))
            ));
            assert_eq!(visited, 1);
            assert_eq!(control.memory().used(), 0);
        }
    }
}
