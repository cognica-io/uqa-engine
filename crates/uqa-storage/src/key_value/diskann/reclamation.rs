//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded deletion uses committed retirement authority; retained readers keep their MVCC history.

use crate::diskann_index::format::DiskANNGeneration;
use crate::key_value::KeyValueRead;
use crate::read_control::StorageReadControl;
use crate::{KeyValueBatch, StorageBackendResult};

use super::keys::{database_key, Keys, Kind};
use super::source::{key_page, KEY_PAGE_LIMIT};
use super::staging::load_state;
use super::state::State;
use super::{invalid, DiskANNStageStatus, KeyValueDiskANNStore};

impl KeyValueDiskANNStore {
    /// Delete at most 64 payload records from a durably retired generation. The final step removes its state. Current heads and unpublished builds cannot be reclaimed here. Retained sources keep the historical pages through ordinary MVCC leases; version reclamation remains separate. Resolve an uncertain original attempt before calling again.
    pub fn reclaim_retired_step(
        &self,
        generation: DiskANNGeneration,
        max_records: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        control.check()?;
        if max_records == 0 {
            return Err(invalid("reclamation requires a positive record limit"));
        }
        let keys = Keys::new(generation);
        let state_key = keys.key(Kind::State);
        let mut complete = false;
        self.mutate(control, &mut |read, batch| {
            let Some(state) = load_state(read, generation, control)? else {
                if read.contains_prefix_budgeted(keys.prefix(), control)? {
                    return Err(invalid("generation has orphaned records"));
                }
                complete = true;
                return Ok(());
            };
            if !matches!(
                state.status,
                DiskANNStageStatus::Retired | DiskANNStageStatus::Discarding
            ) {
                return Err(invalid("generation has no committed reclamation authority"));
            }
            if read
                .record_revision(state_key.as_ref())?
                .and_then(|revision| revision.observed_commit(self.owner.database))
                .is_none()
            {
                return Err(invalid("reclamation requires a committed state revision"));
            }
            complete = delete_page(read, batch, keys, state, max_records, control)?;
            Ok(())
        })?;
        Ok(complete)
    }
}

/// The caller establishes authority on this exact read. State and data identity fence every deletion; no payload is loaded to select its key.
pub(super) fn delete_page(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    keys: Keys,
    state: State,
    max_records: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<bool> {
    let limit = max_records.min(KEY_PAGE_LIMIT);
    let state_key = keys.key(Kind::State);
    let page = key_page(read, keys, Some(state_key.as_ref()), limit, control)?;
    batch.require_unchanged(&database_key())?;
    batch.require_unchanged(state_key.as_ref())?;
    for (key, _) in page.iter() {
        control.check()?;
        batch.delete(key.as_ref())?;
    }
    let complete = page.len() < limit;
    if complete {
        batch.delete(state_key.as_ref())?;
    } else {
        batch.put(
            state_key.as_ref(),
            &State {
                status: DiskANNStageStatus::Discarding,
                ..state
            }
            .encode(),
        )?;
    }
    control.check()?;
    Ok(complete)
}
