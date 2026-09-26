//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped physical tombstone retirement preserves absence observations through epochs.

use uqa_core::memory::BudgetedVec;

use crate::read_control::StorageReadControl;

use super::{CommitSequence, VersionError, VersionResult};

mod conformance;
pub use conformance::{verify_tombstone_reclamation, verify_tombstone_reclamation_pages};
mod maintenance;
pub use maintenance::{reclaim_key_value_diskann_tombstones, reclaim_tombstone_prefix};

/// Autonomous metadata namespaces; providers update them in the same physical transaction as tombstone deletion, without advancing record visibility or transaction allocation.
pub const RECLAMATION_EPOCH_NAMESPACE: &[u8] = b"\0uqa-reclamation-epoch-v1\0";
pub const RECLAMATION_DOMAIN_PREFIX: &[u8] = b"\0uqa-reclamation-domain-v1\0";
pub const TOMBSTONE_RECLAMATION_PAGE: usize = 64;

/// One finite pass keeps its original prefix and visibility cutoff. Later writes cannot join its candidate set; each step visits at most 64 eligible metadata entries. Run ordinary history reclamation first: only fully pruned current tombstones may be retired.
pub struct TombstoneReclamationRequest<'a> {
    pub prefix: &'a [u8],
    pub after: Option<&'a [u8]>,
    pub through: CommitSequence,
}

impl TombstoneReclamationRequest<'_> {
    pub fn validate(&self, control: &StorageReadControl) -> VersionResult<()> {
        control.check()?;
        if self.prefix.is_empty()
            || self.prefix.len() > 1024
            || self.after.is_some_and(|key| !key.starts_with(self.prefix))
        {
            return Err(VersionError::InvalidEncoding(
                "invalid tombstone reclamation range",
            ));
        }
        Ok(())
    }

    pub fn domain_namespace(&self, control: &StorageReadControl) -> VersionResult<BudgetedVec<u8>> {
        self.validate(control)?;
        let mut namespace = BudgetedVec::new(control.memory());
        namespace.extend_from_slice(RECLAMATION_DOMAIN_PREFIX)?;
        namespace.extend_from_slice(self.prefix)?;
        Ok(namespace)
    }
}

pub enum TombstoneReclamationStep {
    /// A live snapshot retains exact revision identities, including deleted rows. Stop this pass and reconsider on the next maintenance invocation.
    Retained,
    Complete {
        removed: usize,
    },
    More {
        after: BudgetedVec<u8>,
        removed: usize,
    },
}

/// Charged metadata for one bounded provider page; graph payloads and old histories never enter this workspace.
pub struct TombstoneReclamationPage {
    keys: BudgetedVec<(BudgetedVec<u8>, u64)>,
    after: BudgetedVec<u8>,
    inspected: usize,
}

impl TombstoneReclamationPage {
    pub fn new(control: &StorageReadControl) -> VersionResult<Self> {
        control.check()?;
        let mut keys = BudgetedVec::new(control.memory());
        keys.reserve(TOMBSTONE_RECLAMATION_PAGE)?;
        Ok(Self {
            keys,
            after: BudgetedVec::new(control.memory()),
            inspected: 0,
        })
    }

    /// Record an eligible identity in key order. Providers may retire only a deleted head whose older history has already been reclaimed.
    pub fn visit(
        &mut self,
        key: &[u8],
        revision: u64,
        retire: bool,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.check()?;
        if self.is_full() || (self.inspected != 0 && key <= &*self.after) {
            return Err(VersionError::InvalidEncoding(
                "invalid tombstone page order",
            ));
        }
        self.after.clear();
        self.after.extend_from_slice(key)?;
        if retire {
            let mut owned = BudgetedVec::new(control.memory());
            owned.extend_from_slice(key)?;
            self.keys.push((owned, revision))?;
        }
        self.inspected += 1;
        Ok(())
    }

    pub fn is_full(&self) -> bool {
        self.inspected == TOMBSTONE_RECLAMATION_PAGE
    }

    pub fn retired(&self) -> impl ExactSizeIterator<Item = (&[u8], u64)> {
        self.keys.iter().map(|(key, revision)| (&**key, *revision))
    }

    pub fn finish(self) -> TombstoneReclamationStep {
        let removed = self.keys.len();
        if self.is_full() {
            TombstoneReclamationStep::More {
                after: self.after,
                removed,
            }
        } else {
            TombstoneReclamationStep::Complete { removed }
        }
    }
}

#[cfg(test)]
mod tests;
