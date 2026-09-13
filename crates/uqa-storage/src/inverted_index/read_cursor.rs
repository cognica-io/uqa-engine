//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed memory postings and retained field lengths for graph candidate traversal.

use super::{
    usize_to_u64, DocId, MemoryInvertedIndex, MemoryPosting, PostingScore, StorageBackendResult,
    TokenTermKey,
};
use crate::{clustered_postings::PostingReadCursor, read_control::StorageReadControl};
use std::{collections::BTreeMap, ops::Bound};
use uqa_core::memory::BudgetedString;

pub(super) struct MemoryPostingReadCursor<'a> {
    index: &'a MemoryInvertedIndex,
    field: &'a str,
    postings: Option<&'a BTreeMap<DocId, MemoryPosting>>,
    current: Option<PostingScore>,
    frequency: u64,
    control: Option<StorageReadControl>,
}

impl<'a> MemoryPostingReadCursor<'a> {
    pub fn new(
        index: &'a MemoryInvertedIndex,
        field: &'a str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Self> {
        let postings = index.index.get(&(field.to_owned(), term.clone()));
        Self::from_postings(index, field, postings, None)
    }

    pub fn with_control(
        index: &'a MemoryInvertedIndex,
        field: &'a str,
        term: &TokenTermKey,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let postings = controlled_postings(index, field, term, control)?;
        Self::from_postings(index, field, postings, Some(control.clone()))
    }

    fn from_postings(
        index: &'a MemoryInvertedIndex,
        field: &'a str,
        postings: Option<&'a BTreeMap<DocId, MemoryPosting>>,
        control: Option<StorageReadControl>,
    ) -> StorageBackendResult<Self> {
        let frequency = usize_to_u64(postings.map_or(0, BTreeMap::len), "document frequency")?;
        let mut cursor = Self {
            index,
            field,
            postings,
            current: None,
            frequency,
            control,
        };
        cursor.seek(Bound::Unbounded)?;
        Ok(cursor)
    }

    fn check(&self) -> StorageBackendResult<()> {
        if let Some(control) = &self.control {
            control.check()?;
        }
        Ok(())
    }

    fn seek(&mut self, start: Bound<DocId>) -> StorageBackendResult<Option<PostingScore>> {
        self.check()?;
        let next = self
            .postings
            .and_then(|postings| postings.range((start, Bound::Unbounded)).next());
        let current = next
            .map(|(&doc_id, posting)| -> StorageBackendResult<PostingScore> {
                Ok(PostingScore {
                    doc_id,
                    term_freq: usize_to_u64(posting.occurrences.len(), "term frequency")?,
                    doc_length: self
                        .index
                        .doc_fields
                        .get(&doc_id)
                        .and_then(|fields| fields.get(self.field))
                        .map_or(0, |metadata| metadata.length),
                })
            })
            .transpose()?;
        self.check()?;
        self.current = current;
        Ok(self.current)
    }
}

impl PostingReadCursor for MemoryPostingReadCursor<'_> {
    fn doc_freq(&self) -> u64 {
        self.frequency
    }
    fn current(&self) -> Option<PostingScore> {
        self.current
    }

    fn advance(&mut self) -> StorageBackendResult<Option<PostingScore>> {
        self.check()?;
        match self.current {
            Some(entry) => self.seek(Bound::Excluded(entry.doc_id)),
            None => Ok(None),
        }
    }

    fn advance_to(&mut self, target: DocId) -> StorageBackendResult<Option<PostingScore>> {
        self.check()?;
        match self.current {
            Some(entry) if entry.doc_id < target => self.seek(Bound::Included(target)),
            _ => Ok(self.current),
        }
    }
}

pub(super) fn controlled_postings<'a>(
    index: &'a MemoryInvertedIndex,
    field: &str,
    term: &TokenTermKey,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<&'a BTreeMap<DocId, MemoryPosting>>> {
    control.check()?;
    let mut field_key = BudgetedString::new(control.memory());
    field_key.reserve(field.len())?;
    for (offset, character) in field.chars().enumerate() {
        if offset % 1024 == 0 {
            control.check()?;
        }
        field_key.push(character)?;
    }
    let term_key = term.clone_budgeted(control.memory(), || control.check())?;
    let (field_key, mut memory) = field_key.into_parts();
    let (term_key, term_memory) = term_key.into_parts();
    memory.absorb(term_memory);
    let key = uqa_core::memory::Budgeted::new((field_key, term_key), memory);
    let postings = index.index.get(&*key);
    control.check()?;
    Ok(postings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InvertedIndex;
    use uqa_core::{
        memory::{MemoryBudget, MemoryError},
        CancellationToken, TokenOccurrence,
    };

    #[test]
    fn controlled_memory_reads_retain_allocations_and_cancel_without_advancing() {
        let mut index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
        for doc_id in [0, 9, DocId::MAX] {
            index
                .add_document(doc_id, BTreeMap::from([("body".into(), "a a".into())]))
                .unwrap();
        }
        let term = TokenTermKey::from_text("a");
        let cancellation = CancellationToken::new();
        let budget = MemoryBudget::new(1 << 20);
        let other = budget.reserve(7).unwrap();
        let control = StorageReadControl::new(&budget, &cancellation);
        let mut cursor = index
            .posting_read_cursor_key_budgeted("body", &term, &control)
            .unwrap();
        assert_eq!(budget.used(), 7 + size_of::<MemoryPostingReadCursor<'_>>());
        assert_eq!(cursor.doc_freq(), 3);
        let occurrences = index
            .get_occurrences_budgeted(0, "body", &term, &control)
            .unwrap();
        assert_eq!(occurrences.len(), 2);
        assert_eq!(
            occurrences.reserved_bytes(),
            2 * size_of::<TokenOccurrence>()
        );
        let retained = budget.used();
        let current = cursor.current();
        cancellation.cancel();
        assert!(matches!(
            cursor.advance(),
            Err(crate::StorageBackendError::Cancelled(_))
        ));
        assert!(matches!(
            cursor.advance_to(0),
            Err(crate::StorageBackendError::Cancelled(_))
        ));
        assert!(matches!(
            index.get_occurrences_budgeted(0, "body", &term, &control),
            Err(crate::StorageBackendError::Cancelled(_))
        ));
        assert_eq!(cursor.current(), current);
        assert_eq!(budget.used(), retained);
        cancellation.reset();
        assert_eq!(cursor.advance().unwrap().unwrap().doc_id, 9);
        assert_eq!(
            cursor.advance_to(DocId::MAX).unwrap().unwrap().doc_id,
            DocId::MAX
        );
        assert_eq!(cursor.advance().unwrap(), None);
        drop(cursor);
        assert_eq!(budget.used(), 7 + occurrences.reserved_bytes());
        drop(occurrences);
        assert_eq!(budget.used(), 7);
        drop(other);
    }

    #[test]
    fn every_memory_read_quota_preserves_independent_owners() {
        let mut index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
        index
            .add_document(9, BTreeMap::from([("body".into(), "a a a".into())]))
            .unwrap();
        let term = TokenTermKey::from_text("a");
        let cancellation = CancellationToken::new();
        let probe = MemoryBudget::new(1 << 20);
        let control = StorageReadControl::new(&probe, &cancellation);
        let cursor = index
            .posting_read_cursor_key_budgeted("body", &term, &control)
            .unwrap();
        let occurrences = index
            .get_occurrences_budgeted(9, "body", &term, &control)
            .unwrap();
        let peak = probe.peak();
        drop(occurrences);
        drop(cursor);
        assert_eq!(probe.used(), 0);
        for limit in 0..=peak {
            let budget = MemoryBudget::new(limit + 7);
            let other = budget.reserve(7).unwrap();
            let control = StorageReadControl::new(&budget, &cancellation);
            let result = (|| -> StorageBackendResult<()> {
                let cursor = index.posting_read_cursor_key_budgeted("body", &term, &control)?;
                let occurrences = index.get_occurrences_budgeted(9, "body", &term, &control)?;
                assert_eq!(cursor.current().unwrap().term_freq, 3);
                assert_eq!(occurrences.len(), 3);
                Ok(())
            })();
            match result {
                Ok(()) => assert!(limit >= peak),
                Err(crate::StorageBackendError::Memory(MemoryError::Limit { .. })) => {
                    assert!(limit < peak);
                }
                result => panic!("unexpected result at quota {limit}: {result:?}"),
            }
            assert_eq!(budget.used(), 7);
            drop(other);
        }
    }

    #[test]
    fn borrowed_cursor_seeks_forward_through_maximum_document_id() {
        let mut index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
        for (doc_id, body) in [(0, "a a"), (5, "b"), (9, "a b"), (DocId::MAX, "a")] {
            index
                .add_document(doc_id, BTreeMap::from([("body".into(), body.into())]))
                .unwrap();
        }
        let mut cursor = index
            .posting_read_cursor_key("body", &TokenTermKey::from_text("a"))
            .unwrap();
        assert_eq!(cursor.doc_freq(), 3);
        assert_eq!(
            cursor.current(),
            Some(PostingScore {
                doc_id: 0,
                term_freq: 2,
                doc_length: 2
            })
        );
        assert_eq!(cursor.advance_to(5).unwrap().unwrap().doc_id, 9);
        assert_eq!(cursor.advance_to(0).unwrap().unwrap().doc_id, 9);
        assert_eq!(cursor.advance().unwrap().unwrap().doc_id, DocId::MAX);
        assert_eq!(cursor.advance().unwrap(), None);
        assert_eq!(cursor.advance_to(0).unwrap(), None);
    }
}
