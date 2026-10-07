//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Indexed references combine committed visibility with private replacements and command staging.

use super::*;
use std::sync::Arc;
use uqa_core::{Payload, PostingEntry, PostingList};
use uqa_storage::{read_control::StorageReadControl, DocumentMetadata, ValueIndexKey};

#[derive(Default)]
struct Source {
    indexed: Option<Vec<DocId>>,
    staged: Vec<DocId>,
}

impl Source {
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
    fn live_table_doc_id_page(
        &self,
        _: &str,
        _: Option<DocId>,
        _: usize,
        _: &StorageReadControl,
    ) -> Result<uqa_core::memory::BudgetedVec<DocId>, SQLError> {
        unreachable!()
    }
    fn get_document(&self, _: &str, _: DocId) -> Result<Option<Document>, SQLError> {
        panic!("candidate lookup read a full payload")
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
    fn staged_expression_matches(
        &self,
        _: &str,
        _: &str,
        _: &[Value],
    ) -> Result<crate::mutation::overlay::CommandIndexProbe, SQLError> {
        unreachable!()
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
    fn doc_ids(&self, _: &str) -> Result<Vec<DocId>, SQLError> {
        panic!("indexed reference enumerated latest documents")
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
    assert_eq!(
        table
            .indexed_doc_ids(&["parent_id".into()], &[Value::Str("wanted".into())])
            .unwrap(),
        Some(vec![3, 4, 5])
    );
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
    assert_eq!(
        table
            .indexed_doc_ids(&["parent_id".into()], &[Value::Str("wanted".into())])
            .unwrap(),
        Some(vec![3, 5, 6])
    );
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
        assert!(table
            .indexed_doc_ids(&["parent_id".into()], &[Value::Str("wanted".into())])
            .unwrap()
            .is_none());
    }
}
