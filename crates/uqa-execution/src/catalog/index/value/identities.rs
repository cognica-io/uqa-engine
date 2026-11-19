//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Imported scalar-null identities retain their original buffer until native mutation or retirement.

use std::sync::Arc;
use uqa_core::{memory::Budgeted, DocId};

pub(super) enum NullIdentities {
    Native(Vec<DocId>),
    Admitted(Arc<Budgeted<Vec<DocId>>>),
}

impl Default for NullIdentities {
    fn default() -> Self {
        Self::Native(Vec::new())
    }
}

impl Clone for NullIdentities {
    fn clone(&self) -> Self {
        Self::Native(self.as_slice().to_vec())
    }
}

impl std::ops::Deref for NullIdentities {
    type Target = [DocId];
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl NullIdentities {
    pub(super) fn as_slice(&self) -> &[DocId] {
        match self {
            Self::Native(ids) => ids,
            Self::Admitted(ids) => ids,
        }
    }

    fn write(&mut self) -> &mut Vec<DocId> {
        if let Self::Admitted(ids) = self {
            *self = Self::Native(ids.to_vec());
        }
        match self {
            Self::Native(ids) => ids,
            Self::Admitted(_) => unreachable!("native identity buffer"),
        }
    }

    pub(super) fn clear(&mut self) {
        match self {
            Self::Native(ids) => ids.clear(),
            Self::Admitted(_) => *self = Self::default(),
        }
    }
    pub(super) fn push(&mut self, id: DocId) {
        self.write().push(id);
    }
    pub(super) fn insert(&mut self, position: usize, id: DocId) {
        self.write().insert(position, id);
    }
    pub(super) fn remove(&mut self, position: usize) {
        self.write().remove(position);
    }
    pub(super) fn sort_unstable(&mut self) {
        self.write().sort_unstable();
    }
    pub(super) fn dedup(&mut self) {
        self.write().dedup();
    }
}
