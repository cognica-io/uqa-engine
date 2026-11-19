//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow immutable native index generations through ordered structural keys.

use super::CatalogIndexRow;
use crate::RelationIdentity;
use std::ops::Bound;

/// The retained owner fixes cardinality, structural keys and rows for every observer's lifetime. Bounds compare structural keys, even when a supplied row contains a different encoded relation.
pub trait CatalogIndexRegistry: std::fmt::Debug + Send + Sync + std::panic::RefUnwindSafe {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn first_from(
        &self,
        start: Bound<&RelationIdentity>,
    ) -> Option<(&RelationIdentity, &CatalogIndexRow)>;
    fn last_before(
        &self,
        end: Bound<&RelationIdentity>,
    ) -> Option<(&RelationIdentity, &CatalogIndexRow)>;
    fn get(&self, key: &RelationIdentity) -> Option<&CatalogIndexRow> {
        self.first_from(Bound::Included(key))
            .filter(|(found, _)| *found == key)
            .map(|(_, row)| row)
    }
}

impl CatalogIndexRegistry for std::collections::BTreeMap<RelationIdentity, CatalogIndexRow> {
    fn len(&self) -> usize {
        self.len()
    }
    fn first_from(
        &self,
        start: Bound<&RelationIdentity>,
    ) -> Option<(&RelationIdentity, &CatalogIndexRow)> {
        self.range((start, Bound::Unbounded)).next()
    }
    fn last_before(
        &self,
        end: Bound<&RelationIdentity>,
    ) -> Option<(&RelationIdentity, &CatalogIndexRow)> {
        self.range((Bound::Unbounded, end)).next_back()
    }
}

/// Constant-space bidirectional observation. The owner supplies bounded ordered seeks; cloning retains only borrowed positions and never copies rows.
#[derive(Clone)]
pub struct CatalogIndexRegistryIter<'a> {
    registry: &'a dyn CatalogIndexRegistry,
    front: Option<&'a RelationIdentity>,
    back: Option<&'a RelationIdentity>,
    remaining: usize,
}

impl<'a> CatalogIndexRegistryIter<'a> {
    pub fn new(registry: &'a dyn CatalogIndexRegistry) -> Self {
        Self {
            registry,
            front: None,
            back: None,
            remaining: registry.len(),
        }
    }
}

impl<'a> Iterator for CatalogIndexRegistryIter<'a> {
    type Item = (&'a RelationIdentity, &'a CatalogIndexRow);
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let entry = self
            .registry
            .first_from(self.front.map_or(Bound::Unbounded, Bound::Excluded))
            .expect("immutable native registry cardinality matches its keys");
        self.front = Some(entry.0);
        self.remaining -= 1;
        Some(entry)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl DoubleEndedIterator for CatalogIndexRegistryIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let entry = self
            .registry
            .last_before(self.back.map_or(Bound::Unbounded, Bound::Excluded))
            .expect("immutable native registry cardinality matches its keys");
        self.back = Some(entry.0);
        self.remaining -= 1;
        Some(entry)
    }
}

impl ExactSizeIterator for CatalogIndexRegistryIter<'_> {}
impl std::iter::FusedIterator for CatalogIndexRegistryIter<'_> {}
