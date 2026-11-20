//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and admitted native rows share lookup, mutation and ordered traversal.

use std::{
    collections::{btree_map, BTreeMap},
    iter::FusedIterator,
    ops::{Bound, Index},
};

use uqa_core::{
    memory::{OwnedMap, OwnedMapRange},
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

    pub(super) fn iter(
        &self,
    ) -> impl ExactSizeIterator<Item = (&DocId, &MemoryDocumentRow)> + FusedIterator {
        match self {
            Self::Ordinary(rows) => DocumentRowIter::Ordinary(rows.iter()),
            Self::Owned(rows) => DocumentRowIter::Owned(rows.iter()),
        }
    }

    pub(super) fn keys(&self) -> impl ExactSizeIterator<Item = &DocId> {
        self.iter().map(|(id, _)| id)
    }

    #[inline]
    pub(super) fn range_from(&self, lower: Bound<&DocId>) -> DocumentRowRange<'_> {
        match self {
            Self::Ordinary(rows) => {
                DocumentRowIter::Ordinary(rows.range((lower, Bound::Unbounded)))
            }
            Self::Owned(rows) => DocumentRowIter::Owned(rows.range_from(lower)),
        }
    }
}

pub(super) type DocumentRowRange<'a> = DocumentRowIter<
    btree_map::Range<'a, DocId, MemoryDocumentRow>,
    OwnedMapRange<'a, DocId, MemoryDocumentRow>,
>;

impl<'a> DocumentRowRange<'a> {
    // A page drains one native iterator. Merge scans advance the same range on demand for each requested ID.
    pub(super) fn visit_page(
        self,
        limit: usize,
        visitor: impl FnMut(DocId, &'a MemoryDocumentRow) -> bool,
    ) -> usize {
        match self {
            Self::Ordinary(rows) => visit_page(rows, limit, visitor),
            Self::Owned(rows) => visit_page(rows, limit, visitor),
        }
    }
}

fn visit_page<'a>(
    rows: impl Iterator<Item = (&'a DocId, &'a MemoryDocumentRow)>,
    limit: usize,
    mut visitor: impl FnMut(DocId, &'a MemoryDocumentRow) -> bool,
) -> usize {
    let mut visited = 0;
    for (&id, row) in rows.take(limit) {
        visited += 1;
        if !visitor(id, row) {
            break;
        }
    }
    visited
}

impl Index<&DocId> for DocumentRows {
    type Output = MemoryDocumentRow;
    fn index(&self, id: &DocId) -> &Self::Output {
        self.get(*id).expect("native document identity")
    }
}

pub(super) enum DocumentRowIter<Ordinary, Owned> {
    Ordinary(Ordinary),
    Owned(Owned),
}

impl<Ordinary: Iterator, Owned: Iterator<Item = Ordinary::Item>> Iterator
    for DocumentRowIter<Ordinary, Owned>
{
    type Item = Ordinary::Item;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Ordinary(rows) => rows.next(),
            Self::Owned(rows) => rows.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Ordinary(rows) => rows.size_hint(),
            Self::Owned(rows) => rows.size_hint(),
        }
    }
}

impl<Ordinary: ExactSizeIterator, Owned: ExactSizeIterator<Item = Ordinary::Item>> ExactSizeIterator
    for DocumentRowIter<Ordinary, Owned>
{
}

impl<Ordinary: FusedIterator, Owned: FusedIterator<Item = Ordinary::Item>> FusedIterator
    for DocumentRowIter<Ordinary, Owned>
{
}
