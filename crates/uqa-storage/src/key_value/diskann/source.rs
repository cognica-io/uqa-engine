//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_core::memory::{BudgetedVec, MemoryReservation};

use crate::diskann_index::format::{DiskANNGeneration, PAGE_BYTES};
use crate::diskann_index::pages::{
    DiskANNPageSource, DiskANNPageVisitor, DiskANNReadCapabilities, DiskANNRecordKey,
    DiskANNRecordVisitor,
};
use crate::key_value::KeyValueRead;
use crate::read_control::StorageReadControl;
use crate::StorageBackendResult;

use super::keys::{Key, Keys, Kind, ROOT};
use super::state::{fixed, State, STATE_BYTES};
use super::{
    invalid, read_data_identity, validate_session, DiskANNStageStatus, KeyValueDiskANNStore,
};

mod owned;
mod selection;

pub(super) const KEY_PAGE_LIMIT: usize = 64;

/// A physical generation pinned to an existing MVCC lease. Opening it reads only fixed metadata; pages and record batches remain lazy.
pub struct KeyValueDiskANNSource {
    pub(in crate::key_value::diskann) read: Arc<dyn KeyValueRead + Send + Sync>,
    generation: DiskANNGeneration,
    query_control: Option<StorageReadControl>,
    _memory: MemoryReservation,
}

impl KeyValueDiskANNSource {
    pub(super) fn capture(
        repository: &KeyValueDiskANNStore,
        generation: DiskANNGeneration,
        status: DiskANNStageStatus,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<Self>> {
        control.check()?;
        let mut state = None;
        {
            let _writer = repository.owner.writer.lock();
            repository.idle(control)?;
            repository.owner.store.with_read_view(&mut |read| {
                state = super::staging::load_state(read, generation, control)?;
                Ok(())
            })?;
        }
        let state = state.ok_or_else(|| invalid("generation state is missing"))?;
        if state.status != status
            && !(status == DiskANNStageStatus::Sealed && state.status.is_complete())
        {
            return Err(invalid(
                "generation has not reached the requested physical state",
            ));
        }
        let lease = if matches!(
            state.status,
            DiskANNStageStatus::Published
                | DiskANNStageStatus::Retired
                | DiskANNStageStatus::Discarding
        ) {
            None
        } else if matches!(state.owner, super::state::StageOwner::Legacy(_)) {
            repository.resume_stage(generation, control)?.lease
        } else {
            Some(
                repository
                    .acquire_owner(
                        state.owner,
                        crate::mvcc::ResourceLeaseRequest::Share,
                        control,
                    )?
                    .ok_or_else(|| invalid("generation recovery is still in progress"))?,
            )
        };
        let store = {
            let _writer = repository.owner.writer.lock();
            repository.idle(control)?;
            let retained = repository
                .owner
                .store
                .open_retained_read_session(control.cancellation())?;
            validate_session(&*repository.owner.store, &*retained)?;
            retained
        };
        let mut read = None;
        store.with_read_view(&mut |view| {
            read = Some(view.retain(&[ROOT])?);
            Ok(())
        })?;
        let mut read = read.ok_or_else(|| invalid("source did not expose a read view"))?;
        if let Some(lease) = lease {
            read = owned::OwnedRead::retain(read, lease, control)?;
        }
        Self::from_read(read, generation, status, None, control)
    }

    fn from_read(
        read: Arc<dyn KeyValueRead + Send + Sync>,
        generation: DiskANNGeneration,
        status: DiskANNStageStatus,
        original: Option<&StorageReadControl>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<Self>> {
        if let Some(original) = original {
            original.check()?;
        }
        read.control().check()?;
        control.check()?;
        let memory = control.memory().reserve(std::mem::size_of::<Self>())?;
        if read_data_identity(&*read, control)? != Some(generation.database()) {
            return Err(invalid("source belongs to another data identity"));
        }
        let key = Keys::new(generation).key(Kind::State);
        let state = fixed(control, |visit| {
            read.visit_value_bounded(key.as_ref(), STATE_BYTES, control, visit)
        })?
        .map(State::decode)
        .transpose()?
        .ok_or_else(|| invalid("generation state is missing"))?;
        if state.status != status
            && !(status == DiskANNStageStatus::Sealed && state.status.is_complete())
        {
            return Err(invalid(
                "generation has not reached the requested physical state",
            ));
        }
        control.check()?;
        Ok(Arc::new(Self {
            read,
            generation,
            query_control: original.cloned(),
            _memory: memory,
        }))
    }

    pub(super) fn keys_after(
        &self,
        after: Option<&[u8]>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<(Key, Kind)>> {
        self.check(control)?;
        let result = key_page(
            &*self.read,
            Keys::new(self.generation),
            after,
            KEY_PAGE_LIMIT,
            control,
        )?;
        self.check(control)?;
        Ok(result)
    }

    pub(super) fn value(
        &self,
        kind: Kind,
        maximum: usize,
        control: &StorageReadControl,
        visit: &mut DiskANNRecordVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.check(control)?;
        let key = Keys::new(self.generation).key(kind);
        let mut seen = false;
        let mut failure = None;
        let source = self
            .read
            .visit_value_bounded(key.as_ref(), maximum, control, &mut |value| {
                if failure.is_some() {
                    return Err(invalid("record visitor already failed"));
                }
                let outcome = (|| {
                    if seen {
                        return Err(invalid("record was returned more than once"));
                    }
                    seen = true;
                    let bytes = value.ok_or_else(|| invalid("generation record is missing"))?;
                    control.check_value_size(bytes.len(), maximum)?;
                    visit(bytes)
                })();
                if let Err(error) = outcome {
                    failure = Some(error);
                    return Err(invalid("record visitor rejected data"));
                }
                Ok(())
            });
        if let Some(error) = failure {
            return Err(error);
        }
        source?;
        if !seen {
            return Err(invalid("record was not returned"));
        }
        self.check(control)
    }

    pub(in crate::key_value::diskann) fn check(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        if let Some(original) = &self.query_control {
            original.check()?;
        }
        self.read.control().check()?;
        control.check()
    }
}

impl DiskANNPageSource for KeyValueDiskANNSource {
    fn generation(&self) -> DiskANNGeneration {
        self.generation
    }
    fn capabilities(&self) -> DiskANNReadCapabilities {
        DiskANNReadCapabilities::new(32, 1).expect("fixed sequential read capability")
    }
    fn read_record(
        &self,
        key: DiskANNRecordKey,
        max_bytes: usize,
        control: &StorageReadControl,
        visit: &mut DiskANNRecordVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.value(Kind::Record(key), max_bytes, control, visit)
    }
    fn read_graph_pages(
        &self,
        pages: &[u64],
        control: &StorageReadControl,
        visit: &mut DiskANNPageVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.check(control)?;
        if pages.len() > self.capabilities().max_batch_pages()
            || pages
                .iter()
                .enumerate()
                .any(|(index, id)| pages[..index].contains(id))
        {
            return Err(invalid(
                "page request exceeds its batch bound or repeats a page",
            ));
        }
        for &id in pages {
            self.value(Kind::Graph(id), PAGE_BYTES, control, &mut |bytes| {
                if bytes.len() != PAGE_BYTES {
                    return Err(invalid("graph page length differs"));
                }
                visit(id, bytes)
            })?;
        }
        self.check(control)
    }
}

/// Copy only a bounded page of fixed-size keys, releasing provider guards before any value decoding or further reads.
pub(super) fn key_page(
    read: &dyn KeyValueRead,
    keys: Keys,
    after: Option<&[u8]>,
    limit: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<(Key, Kind)>> {
    control.check()?;
    let mut page = BudgetedVec::<(Key, Kind)>::new(control.memory());
    let mut failure = None;
    let source = read.visit_keys_after(keys.prefix(), after, limit, control, &mut |bytes| {
        if failure.is_some() {
            return Err(invalid("key visitor already failed"));
        }
        let outcome = (|| {
            control.check()?;
            let previous = page.last().map(|(key, _)| key.as_ref()).or(after);
            if page.len() >= limit || previous.is_some_and(|previous| previous >= bytes) {
                return Err(invalid("key page exceeds its bound or ordering"));
            }
            let kind = keys.decode(bytes)?;
            page.push((keys.key(kind), kind))?;
            Ok(())
        })();
        if let Err(error) = outcome {
            failure = Some(error);
            return Err(invalid("key visitor rejected data"));
        }
        Ok(())
    });
    if let Some(error) = failure {
        return Err(error);
    }
    source?;
    control.check()?;
    Ok(page)
}
