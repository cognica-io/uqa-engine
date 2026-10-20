//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Indexed references combine committed visibility with private replacements and command staging.

use super::*;
use std::sync::Arc;
use uqa_core::{Payload, PostingEntry, PostingList, Predicate};
use uqa_storage::{read_control::StorageReadControl, DocumentMetadata, ValueIndexKey};

#[derive(Default)]
struct Source {
    indexed: Option<Vec<DocId>>,
    staged: Vec<DocId>,
}

impl Source {
    fn page(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> Result<uqa_core::memory::BudgetedVec<DocId>, SQLError> {
        assert!(
            self.indexed.is_none(),
            "indexed reference enumerated documents"
        );
        control.cancellation().check()?;
        let mut page = uqa_core::memory::BudgetedVec::new(control.memory());
        let start = after.map_or(0, |after| self.staged.partition_point(|id| *id <= after));
        for &id in self.staged.iter().skip(start).take(limit) {
            page.push(id).map_err(|error| {
                crate::storage_errors::storage_error("test identity page", &error.into())
            })?;
        }
        Ok(page)
    }

    fn posting(&self) -> Option<PostingList> {
        self.indexed.as_ref().map(|ids| {
            PostingList::from_unsorted(
                ids.iter()
                    .map(|&doc_id| PostingEntry {
                        doc_id,
                        payload: Payload::default(),
                    })
                    .collect(),
            )
        })
    }
}

impl MutationRead for Source {
    fn table_doc_ids(&self, _: &str) -> Result<Vec<DocId>, SQLError> {
        panic!("indexed reference enumerated documents")
    }
    fn live_table_doc_ids(&self, _: &str) -> Result<Vec<DocId>, SQLError> {
        unreachable!()
    }
    fn table_doc_id_page(
        &self,
        _: &str,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> Result<uqa_core::memory::BudgetedVec<DocId>, SQLError> {
        self.page(after, limit, control)
    }
    fn live_table_doc_id_page(
        &self,
        _: &str,
        _: Option<DocId>,
        _: usize,
        _: &StorageReadControl,
    ) -> Result<uqa_core::memory::BudgetedVec<DocId>, SQLError> {
        unreachable!()
    }
    fn get_document(&self, _: &str, id: DocId) -> Result<Option<Document>, SQLError> {
        assert!(
            self.indexed.is_none(),
            "indexed candidate lookup read a full payload"
        );
        Ok(self.staged.contains(&id).then(Document::new))
    }
    fn raw_document(&self, _: &str, _: DocId) -> Result<Option<Document>, SQLError> {
        unreachable!()
    }
    fn command_overlay_changes(&self, _: &str) -> Result<Option<DocumentChanges>, SQLError> {
        unreachable!()
    }
}

impl MutationIndexRead for Source {
    fn index_definitions(
        &self,
    ) -> Result<Arc<crate::catalog::index::physical::PhysicalIndexDefinitions>, SQLError> {
        unreachable!()
    }
    fn find_conflict(&self, _: &str, _: &[String], _: &[Value]) -> Result<Option<DocId>, SQLError> {
        unreachable!()
    }
    fn staged_matches(&self, _: &str, _: &[String], _: &[Value]) -> Result<Vec<DocId>, SQLError> {
        Ok(self.staged.clone())
    }
    fn visit_staged_matches(
        &self,
        _: &str,
        _: &[String],
        _: &[Value],
        visit: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        for &id in &self.staged {
            visit(id)?;
        }
        Ok(())
    }
    fn staged_expression_matches(
        &self,
        _: &str,
        _: &str,
        _: &[Value],
    ) -> Result<crate::mutation::overlay::CommandIndexProbe, SQLError> {
        unreachable!()
    }
    fn visit_value_index_equal(
        &self,
        _: &str,
        _: &ValueIndexKey,
        _: &Value,
        visit: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        let Some(ids) = &self.indexed else {
            return Ok(false);
        };
        for &id in ids {
            visit(id)?;
        }
        Ok(true)
    }
    fn value_index_scan_key(
        &self,
        _: &str,
        _: &ValueIndexKey,
        _: &Predicate,
    ) -> Result<Option<PostingList>, SQLError> {
        Ok(self.posting())
    }
}

impl ReferentialReadSnapshot for Source {
    fn doc_id_page(
        &self,
        _: &str,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> Result<uqa_core::memory::BudgetedVec<DocId>, SQLError> {
        self.page(after, limit, control)
    }

    fn doc_ids(&self, _: &str) -> Result<Vec<DocId>, SQLError> {
        panic!("indexed reference enumerated latest documents")
    }
    fn visit_value_index_equal(
        &self,
        _: &str,
        _: &ValueIndexKey,
        _: &Value,
        visit: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        let Some(ids) = &self.indexed else {
            return Ok(false);
        };
        for &id in ids {
            visit(id)?;
        }
        Ok(true)
    }
    fn value_index_scan_key(
        &self,
        _: &str,
        _: &ValueIndexKey,
        _: &Predicate,
    ) -> Result<Option<PostingList>, SQLError> {
        Ok(self.posting())
    }
    fn document(&self, _: &str, _: DocId) -> Result<Option<Document>, SQLError> {
        unreachable!()
    }
    fn metadata(&self, _: &str, _: DocId) -> Result<Option<DocumentMetadata>, SQLError> {
        Ok(Some(DocumentMetadata::with_tuple_xmin(2)))
    }
}

impl ReferentialSnapshots for Source {
    fn latest_reference_snapshot(&self) -> Result<Box<dyn ReferentialReadSnapshot + '_>, SQLError> {
        unreachable!()
    }
    fn transaction_document_metadata(
        &self,
        _: &str,
        _: DocId,
    ) -> Result<Option<DocumentMetadata>, SQLError> {
        Ok(Some(DocumentMetadata::with_tuple_xmin(1)))
    }
}

fn changes() -> DocumentChanges {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut changes = DocumentChanges::default();
    for (id, value) in [
        (1, None),
        (2, Some("moved")),
        (3, Some("wanted")),
        (5, Some("wanted")),
    ] {
        let row = value.map(|value| {
            (
                Arc::new(Document::from([(
                    "parent_id".into(),
                    Value::Str(value.into()),
                )])),
                DocumentMetadata::default(),
            )
        });
        changes.insert_shared(id, row, &control).unwrap();
    }
    changes
}

#[test]
fn indexed_current_rows_mask_deleted_and_rekeyed_commands() {
    let source = Source {
        indexed: Some(vec![1, 2, 3, 4]),
        staged: vec![3, 5],
    };
    let table = ReferenceTableSnapshot {
        current: &source,
        indexes: &source,
        snapshots: &source,
        latest: None,
        table: "child".into(),
        changes: changes(),
    };
    assert_eq!(selected(&table).unwrap(), Some(vec![3, 4, 5]));
}

#[test]
fn latest_index_adds_commits_and_preserves_only_own_snapshot_rows() {
    let current = Source {
        indexed: Some(vec![1, 2, 3, 4]),
        staged: vec![3, 5],
    };
    let latest = Source {
        indexed: Some(vec![1, 2, 6]),
        staged: vec![],
    };
    let table = ReferenceTableSnapshot {
        current: &current,
        indexes: &current,
        snapshots: &current,
        latest: Some(&latest),
        table: "child".into(),
        changes: changes(),
    };
    assert_eq!(selected(&table).unwrap(), Some(vec![3, 5, 6]));
    assert_eq!(
        table.check_visible(6).unwrap_err().sqlstate(),
        Some("40001")
    );
    table.check_visible(3).unwrap();
}

#[test]
fn missing_index_in_either_generation_requires_the_existing_scan() {
    for (current_ids, latest_ids) in [(None, Some(vec![1])), (Some(vec![1]), None)] {
        let current = Source {
            indexed: current_ids,
            ..Source::default()
        };
        let latest = Source {
            indexed: latest_ids,
            ..Source::default()
        };
        let table = ReferenceTableSnapshot {
            current: &current,
            indexes: &current,
            snapshots: &current,
            latest: Some(&latest),
            table: "child".into(),
            changes: DocumentChanges::default(),
        };
        assert!(selected(&table).unwrap().is_none());
    }
}

fn selected(table: &ReferenceTableSnapshot<'_>) -> Result<Option<Vec<DocId>>, SQLError> {
    table
        .indexed_doc_ids(
            &["parent_id".into()],
            &[Value::Str("wanted".into())],
            64 * 1024,
            &CancellationToken::new(),
        )?
        .map(|ids| {
            let mut found = Vec::new();
            ids.visit(&mut |_, id| {
                found.push(id);
                Ok(true)
            })?;
            Ok(found)
        })
        .transpose()
}

#[test]
fn indexed_reference_candidates_spill_without_copying_complete_postings() {
    let count = 16_384;
    let source = Source {
        indexed: Some((0..count).collect()),
        staged: (0..count).step_by(17).collect(),
    };
    let table = ReferenceTableSnapshot {
        current: &source,
        indexes: &source,
        snapshots: &source,
        latest: None,
        table: "child".into(),
        changes: DocumentChanges::default(),
    };
    let allocated = allocation_counter::measure(|| {
        let ids = table
            .indexed_doc_ids(
                &["parent_id".into()],
                &[Value::Int(1)],
                64 * 1024,
                &CancellationToken::new(),
            )
            .unwrap()
            .unwrap();
        let mut next = 0;
        ids.visit(&mut |table, id| {
            assert!(table.is_empty());
            assert_eq!(id, next);
            next += 1;
            Ok(true)
        })
        .unwrap();
        assert_eq!(next, count);
    });
    assert!(allocated.bytes_max < 512 * 1024, "{allocated:?}");
}

#[test]
fn reference_identity_pages_preserve_fixed_changes_and_early_stop() {
    let current = Source {
        indexed: None,
        staged: vec![3, 5],
    };
    let latest = Source {
        indexed: None,
        staged: vec![1, 2, 6],
    };
    let table = ReferenceTableSnapshot {
        current: &current,
        indexes: &current,
        snapshots: &current,
        latest: Some(&latest),
        table: "child".into(),
        changes: changes(),
    };
    let mut found = Vec::new();
    table
        .visit_doc_ids(0, &CancellationToken::new(), &mut |id| {
            found.push(id);
            Ok(true)
        })
        .unwrap();
    assert_eq!(found, [3, 5, 6]);
    let current = Source {
        indexed: None,
        staged: (0..16_384).collect(),
    };
    let table = ReferenceTableSnapshot {
        current: &current,
        indexes: &current,
        snapshots: &current,
        latest: None,
        table: "child".into(),
        changes: DocumentChanges::default(),
    };
    let allocated = allocation_counter::measure(|| {
        let mut next = 0;
        table
            .visit_doc_ids(64 * 1024, &CancellationToken::new(), &mut |id| {
                assert_eq!(id, next);
                next += 1;
                Ok(next < 7)
            })
            .unwrap();
        assert_eq!(next, 7);
    });
    assert!(allocated.bytes_max < 512 * 1024, "{allocated:?}");
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        table
            .visit_doc_ids(0, &cancel, &mut |_| {
                panic!("cancelled reference scan delivered a candidate")
            })
            .unwrap_err()
            .sqlstate(),
        Some("57014"),
    );
}
