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
    control: &'a StorageReadControl,
    reads: Cell<usize>,
}

impl KeyValueRead for CountedRead<'_> {
    fn control(&self) -> &StorageReadControl {
        self.control
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
    let control = StorageReadControl::with_limit(1 << 20);
    crate::key_value::index_view::read_view(&store, |inner| {
        let counted = CountedRead {
            inner,
            control: &control,
            reads: Cell::new(0),
        };
        let view = || OccurrenceRead {
            store: &counted,
            table: "docs",
            bindings: &bindings,
            format_requires_rebuild: Cell::new(None),
        };
        let mut single = 0;
        for (count, fields) in [
            (1, vec!["body"]),
            (32, vec!["body"]),
            (32, vec!["body", "title"]),
        ] {
            let view = view();
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
            assert_eq!(counted.reads.get(), single + fields.len() - 1);
            assert!(control.memory().peak() > 0);
            assert_eq!(control.memory().used(), 0);
        }
        let held = control.memory().reserve(control.memory().limit()).unwrap();
        assert!(matches!(
            view().stage_documents(
                vec![(1, BTreeMap::from([("body".into(), "alpha beta".into())]))],
                false
            ),
            Err(crate::StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), held.bytes());
        drop(held);
        Ok(())
    })
    .unwrap();
}

#[test]
fn format_admission_is_shared_on_one_fixed_view_and_keeps_cancellation() {
    let store = MemoryKeyValueStore::new();
    let bindings = AnalyzerBindings::new(uqa_analysis::whitespace_analyzer());
    let control = StorageReadControl::with_limit(1 << 20);
    crate::key_value::index_view::read_view(&store, |inner| {
        let counted = CountedRead {
            inner,
            control: &control,
            reads: Cell::new(0),
        };
        let view = OccurrenceRead {
            store: &counted,
            table: "docs",
            bindings: &bindings,
            format_requires_rebuild: Cell::new(None),
        };
        view.require_graph_format()?;
        let initial = counted.reads.get();
        assert!(initial > 0);
        assert!(!view.needs_source_rebuild()?);
        assert_eq!(counted.reads.get(), initial);
        view.stage_documents(
            vec![(
                1,
                BTreeMap::from([
                    ("body".into(), "alpha beta".into()),
                    ("title".into(), "gamma".into()),
                ]),
            )],
            false,
        )?;
        assert_eq!(
            counted.reads.get(),
            initial + 2,
            "each field still validates its stored revision"
        );
        control.cancellation().cancel();
        assert!(matches!(
            view.needs_source_rebuild(),
            Err(crate::StorageBackendError::Cancelled(_))
        ));
        assert_eq!(counted.reads.get(), initial + 2);
        control.cancellation().reset();
        let fresh = OccurrenceRead {
            store: &counted,
            table: "docs",
            bindings: &bindings,
            format_requires_rebuild: Cell::new(None),
        };
        fresh.require_graph_format()?;
        assert_eq!(counted.reads.get(), initial * 2 + 2);
        Ok(())
    })
    .unwrap();
}

#[test]
fn format_admission_keeps_errors_and_does_not_cross_storage_revisions() {
    use crate::key_value::KeyValueStore;
    let store = MemoryKeyValueStore::new();
    let bindings = AnalyzerBindings::new(uqa_analysis::whitespace_analyzer());
    let control = StorageReadControl::with_limit(1 << 20);
    let key = keys::kind_prefix("docs", keys::FORMAT).unwrap();
    for (format, expected) in [
        (b"unsupported".as_slice(), None),
        (b"source-rebuild".as_slice(), Some(true)),
        (keys::FORMAT_NAME, Some(false)),
    ] {
        store.put(&key, format).unwrap();
        crate::key_value::index_view::read_view(&store, |inner| {
            let counted = CountedRead {
                inner,
                control: &control,
                reads: Cell::new(0),
            };
            let view = OccurrenceRead {
                store: &counted,
                table: "docs",
                bindings: &bindings,
                format_requires_rebuild: Cell::new(None),
            };
            let first = view.needs_source_rebuild();
            let reads = counted.reads.get();
            let second = view.needs_source_rebuild();
            if let Some(expected) = expected {
                assert_eq!(first?, expected);
                assert_eq!(second?, expected);
                assert_eq!(counted.reads.get(), reads);
            } else {
                for result in [first, second] {
                    assert!(result
                        .unwrap_err()
                        .to_string()
                        .contains("unsupported occurrence index format"));
                }
                assert_eq!(
                    counted.reads.get(),
                    reads * 2,
                    "failed admission is not cached"
                );
            }
            Ok(())
        })
        .unwrap();
    }
}
