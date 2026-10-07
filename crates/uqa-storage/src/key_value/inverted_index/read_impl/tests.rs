//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Corpus counts consume key identities, including fields with no emitted terms.

use super::*;
use crate::key_value::{
    KeyValueInvertedIndex, KeyValueRead, KeyValueReadRevision, MemoryKeyValueStore,
};
use crate::read_control::{
    KeyReadVisitor, KeyValueReadVisitor, StorageReadControl, ValueReadVisitor,
};
use crate::{InvertedIndex, KeyValueStore};
use std::{cell::Cell, sync::Arc};

struct NoLengths<'a>(&'a dyn KeyValueRead);

impl KeyValueRead for NoLengths<'_> {
    fn control(&self) -> &StorageReadControl {
        self.0.control()
    }
    fn revision(&self, prefixes: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.0.revision(prefixes)
    }
    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        assert!(
            !key.starts_with(&keys::kind_prefix("docs", keys::LENGTH)?),
            "corpus counts must not read length values"
        );
        self.0.visit_value(key, visit)
    }
    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        assert_ne!(
            prefix,
            keys::kind_prefix("docs", keys::LENGTH)?,
            "corpus counts must not read length values"
        );
        self.0.visit_prefix(prefix, visit)
    }
    fn contains_prefix_budgeted(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        self.0.contains_prefix_budgeted(prefix, control)
    }
    fn visit_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.0
            .visit_keys_after(prefix, after, limit, control, visit)
    }
}

#[test]
fn document_count_reads_only_distinct_document_identities() {
    let store = Arc::new(MemoryKeyValueStore::new());
    let analyzer = uqa_analysis::whitespace_analyzer();
    let bindings = crate::inverted_index::AnalyzerBindings::new(analyzer.clone());
    let mut index = KeyValueInvertedIndex::new(store.clone(), "docs", analyzer);
    for (id, fields) in [
        (
            0,
            BTreeMap::from([
                ("a".into(), "alpha".into()),
                ("long_name".into(), "beta".into()),
            ]),
        ),
        (7, BTreeMap::from([("a".into(), String::new())])),
        (
            u64::MAX,
            BTreeMap::from([("long_name".into(), "alpha".into())]),
        ),
    ] {
        index.add_document(id, fields).unwrap();
    }
    let retained = index.snapshot().unwrap();
    for expected in [3, 2] {
        store
            .with_read_view(&mut |inner| {
                let read = NoLengths(inner);
                let view = OccurrenceRead {
                    store: &read,
                    table: "docs",
                    bindings: &bindings,
                    format_requires_rebuild: Cell::new(None),
                };
                assert_eq!(view.doc_count()?, expected);
                Ok(())
            })
            .unwrap();
        index.remove_document(0).unwrap();
    }
    assert_eq!(retained.doc_count().unwrap(), 3);
    assert_eq!(index.doc_count().unwrap(), 2);
}
