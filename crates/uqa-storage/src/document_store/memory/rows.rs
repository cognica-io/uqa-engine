//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and admitted native rows share lookup, mutation and ordered traversal.

use std::{
    collections::{btree_map, BTreeMap},
    ops::{Bound, Index},
};

use uqa_core::{
    memory::{BudgetedMapIter, OwnedMap},
    DocId,
};

use super::MemoryDocumentRow;

#[derive(Debug, Clone)]
pub(super) enum DocumentRows {
    Ordinary(BTreeMap<DocId, MemoryDocumentRow>),
    Owned(OwnedMap<DocId, MemoryDocumentRow>),
}

impl Default for DocumentRows {
    fn default() -> Self {
        Self::Ordinary(BTreeMap::new())
    }
}

impl DocumentRows {
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Ordinary(rows) => rows.len(),
            Self::Owned(rows) => rows.len(),
        }
    }

    pub(super) fn retained_bytes(&self) -> u128 {
        match self {
            Self::Ordinary(rows) => {
                rows.len() as u128 * size_of::<(DocId, MemoryDocumentRow)>() as u128
            }
            Self::Owned(rows) => rows.allocated_bytes() as u128,
        }
    }

    pub(super) fn get(&self, id: DocId) -> Option<&MemoryDocumentRow> {
        match self {
            Self::Ordinary(rows) => rows.get(&id),
            Self::Owned(rows) => rows.get(&id),
        }
    }

    pub(super) fn contains_key(&self, id: DocId) -> bool {
        self.get(id).is_some()
    }

    pub(super) fn insert(
        &mut self,
        id: DocId,
        row: MemoryDocumentRow,
    ) -> Option<MemoryDocumentRow> {
        match self {
            Self::Ordinary(rows) => rows.insert(id, row),
            Self::Owned(rows) => rows.insert(id, row),
        }
    }

    pub(super) fn remove(&mut self, id: DocId) -> Option<MemoryDocumentRow> {
        match self {
            Self::Ordinary(rows) => rows.remove(&id),
            Self::Owned(rows) => rows.remove(&id),
        }
    }

    pub(super) fn iter(&self) -> DocumentRowIter<'_> {
        match self {
            Self::Ordinary(rows) => DocumentRowIter {
                ordinary: Some(rows.iter()),
                owned: None,
            },
            Self::Owned(rows) => DocumentRowIter {
                ordinary: None,
                owned: Some(rows.iter()),
            },
        }
    }

    pub(super) fn keys(&self) -> impl ExactSizeIterator<Item = &DocId> {
        self.iter().map(|(id, _)| id)
    }

    pub(super) fn range_from(
        &self,
        lower: Bound<&DocId>,
    ) -> impl Iterator<Item = (&DocId, &MemoryDocumentRow)> {
        let (ordinary, owned) = match self {
            Self::Ordinary(rows) => (Some(rows.range((lower, Bound::Unbounded))), None),
            Self::Owned(rows) => (None, Some(rows.range_from(lower))),
        };
        ordinary
            .into_iter()
            .flatten()
            .chain(owned.into_iter().flatten())
    }
}

impl Index<&DocId> for DocumentRows {
    type Output = MemoryDocumentRow;
    fn index(&self, id: &DocId) -> &Self::Output {
        self.get(*id).expect("native document identity")
    }
}

// Only one native cursor is active. Both stay on the stack, and complete row collection retains its original exact capacity hint.
pub(super) struct DocumentRowIter<'a> {
    ordinary: Option<btree_map::Iter<'a, DocId, MemoryDocumentRow>>,
    owned: Option<BudgetedMapIter<'a, DocId, MemoryDocumentRow>>,
}

impl<'a> Iterator for DocumentRowIter<'a> {
    type Item = (&'a DocId, &'a MemoryDocumentRow);

    fn next(&mut self) -> Option<Self::Item> {
        self.ordinary
            .as_mut()
            .and_then(Iterator::next)
            .or_else(|| self.owned.as_mut().and_then(Iterator::next))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.ordinary.as_ref().map_or(0, ExactSizeIterator::len)
            + self.owned.as_ref().map_or(0, ExactSizeIterator::len);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for DocumentRowIter<'_> {}
impl std::iter::FusedIterator for DocumentRowIter<'_> {}
