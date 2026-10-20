//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{memory::BudgetedVec, Value};
use uqa_storage::{document_store::Document, StorageBackendResult, StoredDocument};

struct IdentitySource {
    count: u64,
    borrowed: bool,
    pages: AtomicUsize,
}

impl DocumentStore for IdentitySource {
    fn put_stored(&mut self, _: DocId, _: StoredDocument) -> StorageBackendResult<()> {
        panic!("write")
    }
    fn get_stored(&self, _: DocId) -> StorageBackendResult<Option<StoredDocument>> {
        panic!("candidate capture must not read a row payload")
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        panic!("write")
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        panic!("write")
    }
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        panic!("whole-relation identity collection")
    }
    fn len(&self) -> StorageBackendResult<usize> {
        panic!("candidate capture does not need a count")
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        panic!("replacing the selected snapshot")
    }
    fn for_each_next_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        if !self.borrowed {
            return Ok(None);
        }
        assert!(fields.is_empty());
        assert!(limit <= ID_PAGE_SIZE);
        self.pages.fetch_add(1, Ordering::Relaxed);
        let mut visited = 0;
        for id in (after.unwrap_or(0)..self.count)
            .take(limit)
            .map(|id| id + 1)
        {
            visited += 1;
            if !visitor(id, &[]) {
                break;
            }
        }
        Ok(Some(visited))
    }
    fn next_doc_ids_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        assert!(!self.borrowed);
        assert!(limit <= ID_PAGE_SIZE);
        self.pages.fetch_add(1, Ordering::Relaxed);
        let mut ids = BudgetedVec::new(control.memory());
        for id in (after.unwrap_or(0)..self.count)
            .take(limit)
            .map(|id| id + 1)
        {
            control.check()?;
            ids.push(id)?;
        }
        Ok(ids)
    }
}

#[test]
fn mutation_candidate_capture_bounds_borrowed_identity_pages_and_retention() {
    assert_bounded_capture(true);
}

#[test]
fn mutation_candidate_capture_bounds_controlled_identity_pages_and_retention() {
    assert_bounded_capture(false);
}

fn assert_bounded_capture(borrowed: bool) {
    const COUNT: u64 = 16_384;
    let mut source = IdentitySource {
        count: COUNT,
        borrowed,
        pages: AtomicUsize::new(0),
    };
    let changes = DocumentChanges::from_rows(
        BTreeMap::from([
            (1, None),
            (
                2,
                Some(StoredDocument::with_metadata(
                    Document::new(),
                    uqa_storage::DocumentMetadata::default(),
                )),
            ),
            (
                COUNT + 1,
                Some(StoredDocument::with_metadata(
                    Document::new(),
                    uqa_storage::DocumentMetadata::default(),
                )),
            ),
        ]),
        &StorageReadControl::with_limit(64 * 1024),
    )
    .unwrap();
    let allocation = allocation_counter::measure(|| {
        let mut candidates = CandidateIds::new(64 * 1024, CancellationToken::new());
        candidates.capture_table(0, &source, &changes).unwrap();
        candidates.capture_table(1, &source, &changes).unwrap();
        candidates.push(1, DocId::MAX).unwrap();
        let captured = candidates.finish().unwrap();
        assert!(captured.rows.has_spilled());
        // Mutation must not affect an already selected command's candidate sequence.
        source.count = 0;
        for _ in 0..2 {
            let mut rows = captured.read_rows().unwrap();
            for table in 0..2 {
                for id in 2..=COUNT + 1 {
                    assert_eq!(rows.next().unwrap().unwrap(), (table, id));
                }
            }
            assert_eq!(rows.next().unwrap().unwrap(), (1, DocId::MAX));
            assert!(rows.next().is_none());
        }
    });
    assert!(allocation.bytes_max < 512 * 1024, "{allocation:?}");
    // A bounded page scan reads each source range once, including the final empty page.
    assert!(source.pages.load(Ordering::Relaxed) <= 2 * (COUNT as usize / ID_PAGE_SIZE + 3));
}

#[test]
fn mutation_candidate_replay_preserves_repeated_and_full_width_identities() {
    for bytes in [0, 64, 64 * 1024] {
        let mut candidates = CandidateIds::new(bytes, CancellationToken::new());
        let expected = [(usize::MAX, DocId::MAX), (0, 0), (usize::MAX, DocId::MAX)];
        for (table, id) in expected {
            candidates.push(table, id).unwrap();
        }
        let captured = candidates.finish().unwrap();
        assert_eq!(
            captured
                .read_rows()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            expected
        );
    }
    let captured = CandidateIds::new(0, CancellationToken::new())
        .finish()
        .unwrap();
    assert!(captured.read_rows().unwrap().next().is_none());
}

#[test]
fn mutation_candidate_capture_and_replay_stop_on_cancellation() {
    let token = CancellationToken::new();
    let mut candidates = CandidateIds::new(0, token.clone());
    candidates.push(0, 1).unwrap();
    let captured = candidates.finish().unwrap();
    let mut reader = captured.read_rows().unwrap();
    let mut pending = CandidateIds::new(64, token.clone());
    let source = IdentitySource {
        count: 128,
        borrowed: false,
        pages: AtomicUsize::new(0),
    };
    token.cancel();
    assert_eq!(
        reader.next().unwrap().unwrap_err().sqlstate(),
        Some("57014")
    );
    assert_eq!(
        captured.read_rows().err().unwrap().sqlstate(),
        Some("57014")
    );
    assert_eq!(pending.push(0, 2).unwrap_err().sqlstate(), Some("57014"));
    assert_eq!(
        pending
            .capture_table(0, &source, &DocumentChanges::default())
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(source.pages.load(Ordering::Relaxed), 0);
    assert_eq!(pending.finish().err().unwrap().sqlstate(), Some("57014"));
}
