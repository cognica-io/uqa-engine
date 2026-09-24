//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One fixed read boundary validates each field revision for the whole batch.

use std::cell::Cell;

use super::*;
use crate::key_value::{KeyValueRead, KeyValueReadRevision, MemoryKeyValueStore};
use crate::read_control::{KeyValueReadVisitor, StorageReadControl, ValueReadVisitor};

struct CountedRead<'a> {
    inner: &'a dyn KeyValueRead,
    reads: Cell<usize>,
}

impl KeyValueRead for CountedRead<'_> {
    fn control(&self) -> &StorageReadControl {
        self.inner.control()
    }
    fn revision(&self, prefixes: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.inner.revision(prefixes)
    }
    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.reads.set(self.reads.get() + 1);
        self.inner.visit_value(key, visit)
    }
    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.reads.set(self.reads.get() + 1);
        self.inner.visit_prefix(prefix, visit)
    }
    fn contains_prefix_budgeted(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        self.reads.set(self.reads.get() + 1);
        self.inner.contains_prefix_budgeted(prefix, control)
    }
}

#[test]
fn revision_reads_depend_on_fields_instead_of_document_count() {
    let store = MemoryKeyValueStore::new();
    let bindings = AnalyzerBindings::new(uqa_analysis::whitespace_analyzer());
    crate::key_value::index_view::read_view(&store, |inner| {
        let counted = CountedRead {
            inner,
            reads: Cell::new(0),
        };
        let view = OccurrenceRead {
            store: &counted,
            table: "docs",
            bindings: &bindings,
        };
        let mut single = 0;
        for (count, fields) in [
            (1, vec!["body"]),
            (32, vec!["body"]),
            (32, vec!["body", "title"]),
        ] {
            counted.reads.set(0);
            let staged = view.stage_documents(
                (0..count)
                    .map(|doc| {
                        (
                            doc,
                            fields
                                .iter()
                                .map(|field| ((*field).into(), "alpha beta alpha".into()))
                                .collect(),
                        )
                    })
                    .collect(),
                false,
            )?;
            assert_eq!(staged.len(), count as usize);
            for document in staged.values() {
                assert_eq!(document.len(), fields.len());
                for field in document.values() {
                    assert_eq!(field.metadata.length, 3);
                    assert_eq!(field.terms.len(), 2);
                    assert_eq!(field.terms[&TokenTermKey::from_text("alpha")].len(), 2);
                }
            }
            if count == 1 {
                single = counted.reads.get();
                assert!(single > 0);
            }
            assert_eq!(counted.reads.get(), single * fields.len());
        }
        Ok(())
    })
    .unwrap();
}
