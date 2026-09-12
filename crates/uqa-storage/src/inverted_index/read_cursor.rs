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
use crate::clustered_postings::PostingReadCursor;
use std::{collections::BTreeMap, ops::Bound};

pub(super) struct MemoryPostingReadCursor<'a> {
    index: &'a MemoryInvertedIndex,
    field: &'a str,
    postings: Option<&'a BTreeMap<DocId, MemoryPosting>>,
    current: Option<PostingScore>,
    frequency: u64,
}

impl<'a> MemoryPostingReadCursor<'a> {
    pub fn new(
        index: &'a MemoryInvertedIndex,
        field: &'a str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Self> {
        let postings = index.index.get(&(field.to_owned(), term.clone()));
        let frequency = usize_to_u64(postings.map_or(0, BTreeMap::len), "document frequency")?;
        let mut cursor = Self {
            index,
            field,
            postings,
            current: None,
            frequency,
        };
        cursor.seek(Bound::Unbounded)?;
        Ok(cursor)
    }

    fn seek(&mut self, start: Bound<DocId>) -> StorageBackendResult<Option<PostingScore>> {
        let next = self
            .postings
            .and_then(|postings| postings.range((start, Bound::Unbounded)).next());
        self.current = next
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
        match self.current {
            Some(entry) => self.seek(Bound::Excluded(entry.doc_id)),
            None => Ok(None),
        }
    }

    fn advance_to(&mut self, target: DocId) -> StorageBackendResult<Option<PostingScore>> {
        match self.current {
            Some(entry) if entry.doc_id < target => self.seek(Bound::Included(target)),
            _ => Ok(self.current),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InvertedIndex;

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
