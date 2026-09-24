//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical skip rows retain their existing values and canonical common addresses.

use super::*;
use crate::{Catalog, ManagedConnection};
use std::collections::BTreeMap;
use uqa_storage::TokenTermKey;

#[test]
fn captured_skip_offsets_match_the_physical_index_after_source_clear() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut index =
        SQLiteInvertedIndex::new(connection, "articles", uqa_analysis::whitespace_analyzer());
    index
        .try_add_documents(
            (1..=257)
                .map(|id| (id, BTreeMap::from([("body".into(), "alpha".into())])))
                .collect(),
        )
        .unwrap();
    index.flush_skip_pointers().unwrap();
    let (document, offset) = index.skip_to("body", "alpha", 200).unwrap();
    assert!(document > 0 && offset > 0);
    let control = StorageReadControl::with_limit(1 << 20);
    let prefix = Address::table("articles").encode(&control).unwrap();
    let retained = index
        .conn
        .with(|connection| {
            read_snapshot(connection, |connection| {
                PhysicalRead {
                    connection,
                    table: "articles",
                    control: &control,
                    revision: KeyValueReadRevision::fresh(),
                }
                .retain(&[&prefix])
                .map_err(SQLiteError::from)
            })
        })
        .unwrap();
    index.clear().unwrap();
    drop(index);
    let term = TokenTermKey::from_text("alpha");
    let mut address = Address::table("articles");
    address.projection = Some(Projection::Skip);
    address.field = Some("body");
    address.term = Some(term.as_bytes());
    address.document = Some(document);
    let key = address.encode(&control).unwrap();
    let bytes = retained.get(&key).unwrap().unwrap();
    assert_eq!(
        u64::from_be_bytes(bytes[..].try_into().unwrap()),
        offset as u64
    );
    drop((bytes, key, prefix, retained));
    assert_eq!(control.memory().used(), 0);
}
