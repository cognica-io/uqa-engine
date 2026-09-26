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
use super::state::{StageOwner, State};
use super::{invalid, DiskANNStageStatus, KeyValueDiskANNStore};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Reclamation {
    Complete,
    More,
    Retained,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ReclamationMode {
    Explicit,
    Maintenance,
}

impl KeyValueDiskANNStore {
    /// Reclaim an unpublished generation only after exclusively acquiring its abandoned physical owner. A live build, retained sealed source or uncertain original attempt returns `false` without deleting anything. Each successful step deletes at most 64 payload records; partial cleanup persists Discarding and can resume through either reclamation method. Current published heads are always rejected.
    pub fn reclaim_abandoned_step(
        &self,
        generation: DiskANNGeneration,
        max_records: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        self.reclaim_generation(generation, max_records, ReclamationMode::Explicit, control)
            .map(|result| result == Reclamation::Complete)
    }

    pub(super) fn reclaim_generation(
        &self,
        generation: DiskANNGeneration,
        max_records: usize,
        mode: ReclamationMode,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Reclamation> {
        control.check()?;
        if max_records == 0 {
            return Err(invalid("reclamation requires a positive record limit"));
        }
        let mut observed = None;
        {
            let _writer = self.owner.writer.lock();
            self.idle(control)?;
            self.owner.store.with_read_view(&mut |read| {
                observed = load_state(read, generation, control)?;
                Ok(())
            })?;
        }
        let Some(state) = observed else {
            return self
                .reclaim_retired_step(generation, max_records, control)
                .map(Reclamation::from);
        };
        match state.status {
            DiskANNStageStatus::Published => {
                if mode == ReclamationMode::Maintenance {
                    return Ok(Reclamation::Retained);
                }
                return Err(invalid("published generation cannot be abandoned"));
            }
            DiskANNStageStatus::Retired | DiskANNStageStatus::Discarding => {
                return self
                    .reclaim_retired_step(generation, max_records, control)
                    .map(Reclamation::from);
            }
            DiskANNStageStatus::Writing
            | DiskANNStageStatus::Frozen
            | DiskANNStageStatus::Sealed => {}
        }
        let lease = match state.owner {
            StageOwner::Legacy(_) => {
                let Some(legacy) = self.legacy_guard(control)? else {
                    return Ok(Reclamation::Retained);
                };
                let lease = self.reserve_owner(control)?;
                Self::retain_transition(&lease, legacy, control)?
            }
            StageOwner::Leased(_) => {
                let Some(lease) = self.acquire_owner(
                    state.owner,
                    crate::mvcc::ResourceLeaseRequest::Recover,
                    control,
                )?
                else {
                    return Ok(Reclamation::Retained);
                };
                lease
            }
        };
        let keys = Keys::new(generation);
        let mut result = Reclamation::Retained;
        self.mutate_owned(Some(&lease), control, &mut |read, batch| {
            if load_state(read, generation, control)? != Some(state) {
                if mode == ReclamationMode::Maintenance {
                    return Ok(());
                }
                return Err(invalid(
                    "generation changed during abandoned-owner acquisition",
                ));
            }
            if read
                .record_revision(keys.key(Kind::State).as_ref())?
                .and_then(|revision| revision.observed_commit(self.owner.database))
                .is_none()
            {
                return Err(invalid("abandonment requires a committed state revision"));
            }
            result = Reclamation::from(delete_page(
                read,
                batch,
                keys,
                State {
                    owner: StageOwner::Leased(lease.id().allocation()),
                    ..state
                },
                max_records,
                control,
            )?);
            Ok(())
        })?;
        Ok(result)
    }

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

impl From<bool> for Reclamation {
    fn from(complete: bool) -> Self {
        if complete {
            Self::Complete
        } else {
            Self::More
        }
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
