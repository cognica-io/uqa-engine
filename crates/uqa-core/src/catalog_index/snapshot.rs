//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable observations of already-owned index catalog rows.

use super::CatalogIndexRow;
use crate::RelationIdentity;
use std::{collections::BTreeMap, sync::Arc};

/// Retain an immutable catalog generation without copying row payloads or keeping a catalog lock. Native registries enumerate their structural keys; an owned row sequence preserves its supplied order, including repeated identities. This observation does not claim admission for the catalog's original construction.
#[derive(Clone, Debug, Default)]
pub struct CatalogIndexSnapshot(Option<Storage>);

#[derive(Clone, Debug)]
enum Storage {
    Registry(Arc<BTreeMap<RelationIdentity, CatalogIndexRow>>),
    Rows(Arc<Vec<CatalogIndexRow>>),
}

impl From<Arc<BTreeMap<RelationIdentity, CatalogIndexRow>>> for CatalogIndexSnapshot {
    fn from(rows: Arc<BTreeMap<RelationIdentity, CatalogIndexRow>>) -> Self {
        Self(Some(Storage::Registry(rows)))
    }
}

impl From<Vec<CatalogIndexRow>> for CatalogIndexSnapshot {
    fn from(rows: Vec<CatalogIndexRow>) -> Self {
        if rows.is_empty() {
            Self::default()
        } else {
            Self(Some(Storage::Rows(Arc::new(rows))))
        }
    }
}

impl CatalogIndexSnapshot {
    pub fn iter(
        &self,
    ) -> impl ExactSizeIterator<Item = &CatalogIndexRow> + DoubleEndedIterator + Clone {
        match &self.0 {
            Some(Storage::Registry(rows)) => Rows::Registry(rows.values()),
            Some(Storage::Rows(rows)) => Rows::Sequence(rows.iter()),
            None => Rows::Sequence([].iter()),
        }
    }

    pub fn len(&self) -> usize {
        self.iter().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone)]
enum Rows<'a> {
    Registry(std::collections::btree_map::Values<'a, RelationIdentity, CatalogIndexRow>),
    Sequence(std::slice::Iter<'a, CatalogIndexRow>),
}

impl<'a> Iterator for Rows<'a> {
    type Item = &'a CatalogIndexRow;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Registry(rows) => rows.next(),
            Self::Sequence(rows) => rows.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Registry(rows) => rows.size_hint(),
            Self::Sequence(rows) => rows.size_hint(),
        }
    }
}

impl DoubleEndedIterator for Rows<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Registry(rows) => rows.next_back(),
            Self::Sequence(rows) => rows.next_back(),
        }
    }
}

impl ExactSizeIterator for Rows<'_> {}

#[cfg(test)]
mod tests;
