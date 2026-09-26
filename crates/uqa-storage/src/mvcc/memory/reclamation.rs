//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::ops::Bound::{Excluded, Included, Unbounded};

use uqa_core::memory::BudgetedVec;

use crate::mvcc::{
    TombstoneReclamationRequest, TombstoneReclamationStep, VersionError, VersionResult,
    TOMBSTONE_RECLAMATION_PAGE,
};
use crate::read_control::StorageReadControl;

use super::{MemoryReservation, MemoryVersionStore, RecordKey};

impl MemoryVersionStore {
    /// Reference implementation of atomic prefix retirement, sharing snapshot and write admission. Every allocation completes before published state is changed.
    pub fn reclaim_tombstones(
        &self,
        request: &TombstoneReclamationRequest<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<TombstoneReclamationStep> {
        request.validate(control)?;
        let mut state = self.database.state.lock();
        if !state.snapshots.is_empty() {
            return Ok(TombstoneReclamationStep::Retained);
        }
        if request.through > state.sequence {
            return Err(VersionError::InvalidEncoding(
                "tombstone cutoff exceeds committed visibility",
            ));
        }
        let mut keys = BudgetedVec::new(control.memory());
        keys.reserve(TOMBSTONE_RECLAMATION_PAGE)?;
        let mut inspected = 0;
        let mut after = BudgetedVec::new(control.memory());
        let start = request.after.map_or(Included(request.prefix), Excluded);
        for (key, entry) in state.records.range::<[u8], _>((start, Unbounded)) {
            control.check()?;
            if !key.bytes().starts_with(request.prefix) {
                break;
            }
            let head = entry
                .history
                .head()
                .ok_or(VersionError::InvalidEncoding("record head has no history"))?;
            if head.sequence() > request.through {
                continue;
            }
            inspected += 1;
            after.clear();
            after.extend_from_slice(key.bytes())?;
            if head.value().is_none() && entry.history.len() == 1 {
                keys.push(key.clone())?;
            }
            if inspected == TOMBSTONE_RECLAMATION_PAGE {
                break;
            }
        }
        let removed = keys.len();
        if removed != 0 {
            let epoch = state
                .reclamation_epoch
                .checked_add(1)
                .ok_or(VersionError::IdentifiersExhausted)?;
            let domain = if state.reclamation_domains.contains_key(request.prefix) {
                None
            } else {
                Some((
                    RecordKey::new(request.prefix, &self.database.memory)?,
                    self.database
                        .memory
                        .reserve(std::mem::size_of::<(RecordKey, (u64, MemoryReservation))>())?,
                ))
            };
            control.check()?;
            if let Some((prefix, memory)) = domain {
                state.reclamation_domains.insert(prefix, (epoch, memory));
            } else {
                state
                    .reclamation_domains
                    .get_mut(request.prefix)
                    .expect("existing domain")
                    .0 = epoch;
            }
            state.reclamation_epoch = epoch;
            for key in keys.iter() {
                state.records.remove(key.bytes());
            }
        }
        Ok(if inspected == TOMBSTONE_RECLAMATION_PAGE {
            TombstoneReclamationStep::More { after, removed }
        } else {
            TombstoneReclamationStep::Complete { removed }
        })
    }
}
