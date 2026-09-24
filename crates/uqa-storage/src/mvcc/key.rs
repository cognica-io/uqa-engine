//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared immutable identities retain the allocation charged to their owner.

use std::borrow::Borrow;
use std::sync::Arc;

use uqa_core::memory::{BudgetedVec, MemoryBudget};

use super::VersionResult;

#[derive(Debug, Clone)]
pub(super) struct RecordKey(Arc<BudgetedVec<u8>>);

impl RecordKey {
    pub(super) fn new(bytes: &[u8], memory: &MemoryBudget) -> VersionResult<Self> {
        let mut owned = BudgetedVec::new(memory);
        owned.extend_from_slice(bytes)?;
        Ok(Self(Arc::new(owned)))
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Borrow<[u8]> for RecordKey {
    fn borrow(&self) -> &[u8] {
        self.bytes()
    }
}

impl PartialEq for RecordKey {
    fn eq(&self, other: &Self) -> bool {
        self.bytes() == other.bytes()
    }
}
impl Eq for RecordKey {}
impl PartialOrd for RecordKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RecordKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.bytes().cmp(other.bytes())
    }
}
