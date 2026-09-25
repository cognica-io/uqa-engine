//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_core::memory::BudgetedVec;

use crate::diskann_index::format::{DiskANNGeneration, DiskANNManifest, PAGE_BYTES};
use crate::diskann_index::pages::{read_record, DiskANNArtifactSealer, DiskANNRecordKey};
use crate::key_value::KeyValueRead;
use crate::read_control::StorageReadControl;
use crate::{KeyValueBatch, StorageBackendResult};

use super::keys::{database_key, Keys, Kind};
use super::source::{key_page, KEY_PAGE_LIMIT};
use super::state::{fixed, State, STATE_BYTES};
use super::{
    invalid, read_data_identity, DiskANNStageStatus, KeyValueDiskANNSource, KeyValueDiskANNStore,
};

/// A reserved generation and its recoverable staging owner. Discard never recreates a consumed generation number, even through an old handle.
pub struct KeyValueDiskANNStage {
    repository: KeyValueDiskANNStore,
    generation: DiskANNGeneration,
    owner: [u8; 16],
    attempted: bool,
}

impl KeyValueDiskANNStage {
    pub(super) fn reserved(
        repository: KeyValueDiskANNStore,
        generation: DiskANNGeneration,
        owner: [u8; 16],
    ) -> Self {
        Self {
            repository,
            generation,
            owner,
            attempted: false,
        }
    }

    pub(super) fn resume(
        repository: KeyValueDiskANNStore,
        generation: DiskANNGeneration,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut state = None;
        {
            let _writer = repository.owner.writer.lock();
            repository.idle(control)?;
            repository.owner.store.with_read_view(&mut |read| {
                state = load_state(read, generation, control)?;
                Ok(())
            })?;
        }
        let state = state.ok_or_else(|| invalid("cannot resume an absent generation"))?;
        Ok(Self {
            repository,
            generation,
            owner: state.owner,
            attempted: true,
        })
    }

    pub fn generation(&self) -> DiskANNGeneration {
        self.generation
    }

    pub fn status(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNStageStatus>> {
        let _writer = self.repository.owner.writer.lock();
        self.repository.idle(control)?;
        let mut result = None;
        self.repository.owner.store.with_read_view(&mut |read| {
            result = self.state(read, control)?.map(|state| state.status);
            Ok(())
        })?;
        Ok(result)
    }

    pub fn start(&mut self, control: &StorageReadControl) -> StorageBackendResult<()> {
        let create = !self.attempted;
        self.attempted = true;
        let keys = Keys::new(self.generation);
        self.repository.mutate(control, &mut |read, batch| {
            match self.state(read, control)? {
                Some(state) if state.status == DiskANNStageStatus::Writing => return Ok(()),
                Some(_) => return Err(invalid("generation is no longer writable")),
                None if !create => {
                    return Err(invalid("a consumed generation cannot be recreated"))
                }
                None => {}
            }
            if read.contains_prefix_budgeted(keys.prefix(), control)? {
                return Err(invalid("generation has orphaned records"));
            }
            batch.require_unchanged(&database_key())?;
            batch.put(
                keys.key(Kind::State).as_ref(),
                &State {
                    status: DiskANNStageStatus::Writing,
                    owner: self.owner,
                }
                .encode(),
            )
        })
    }

    /// Store one already encoded batch. The provider's existing private-write allowance owns its retained copy.
    pub fn write_record(
        &self,
        key: DiskANNRecordKey,
        bytes: &[u8],
        max_bytes: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check_value_size(bytes.len(), max_bytes)?;
        if key == DiskANNRecordKey::Manifest {
            return Err(invalid(
                "manifest is written only when freezing a generation",
            ));
        }
        self.write(Kind::Record(key), bytes, control)
    }

    pub fn write_graph_page(
        &self,
        id: u64,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        if bytes.len() != PAGE_BYTES {
            return Err(invalid("graph page length differs"));
        }
        self.write(Kind::Graph(id), bytes, control)
    }

    fn write(
        &self,
        kind: Kind,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let keys = Keys::new(self.generation);
        self.repository.mutate(control, &mut |read, batch| {
            let state = self.require_state(read, control)?;
            if state.status != DiskANNStageStatus::Writing {
                return Err(invalid("generation is no longer writable"));
            }
            let key = keys.key(kind);
            if read.contains_prefix_budgeted(key.as_ref(), control)? {
                return Err(invalid("staging records cannot be replaced"));
            }
            self.fence(batch)?;
            batch.put(key.as_ref(), bytes)
        })
    }

    /// Freeze immutable records, validate every stored stream, then mark their physical seal. This does not publish a catalog or establish canonical snapshot coverage.
    pub fn seal(
        &self,
        manifest: DiskANNManifest,
        max_record_bytes: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<KeyValueDiskANNSource>> {
        control.check()?;
        if manifest.input().generation != self.generation {
            return Err(invalid("manifest belongs to another generation"));
        }
        let bytes = manifest.encode(control)?;
        control.check_value_size(bytes.len(), max_record_bytes)?;
        let keys = Keys::new(self.generation);
        let manifest_key = keys.key(Kind::Record(DiskANNRecordKey::Manifest));
        let mut already_sealed = false;
        self.repository.mutate(control, &mut |read, batch| {
            let state = self.require_state(read, control)?;
            match state.status {
                DiskANNStageStatus::Writing => {
                    if read.contains_prefix_budgeted(manifest_key.as_ref(), control)? {
                        return Err(invalid("writable generation already has a manifest"));
                    }
                    self.fence(batch)?;
                    batch.put(manifest_key.as_ref(), &bytes)?;
                    self.set_status(batch, DiskANNStageStatus::Frozen)
                }
                DiskANNStageStatus::Frozen | DiskANNStageStatus::Sealed => {
                    verify_manifest(read, keys, &bytes, control)?;
                    already_sealed = state.status == DiskANNStageStatus::Sealed;
                    Ok(())
                }
                DiskANNStageStatus::Discarding => Err(invalid("generation is being discarded")),
            }
        })?;
        if already_sealed {
            return self.repository.open_source(self.generation, control);
        }
        let source = KeyValueDiskANNSource::capture(
            &self.repository,
            self.generation,
            DiskANNStageStatus::Frozen,
            control,
        )?;
        verify_streams(&source, &manifest, max_record_bytes, control)?;
        self.repository.mutate(control, &mut |read, batch| {
            let state = self.require_state(read, control)?;
            if !matches!(
                state.status,
                DiskANNStageStatus::Frozen | DiskANNStageStatus::Sealed
            ) {
                return Err(invalid("generation changed during physical verification"));
            }
            verify_manifest(read, keys, &bytes, control)?;
            if state.status == DiskANNStageStatus::Frozen {
                self.fence(batch)?;
                batch.require_unchanged(manifest_key.as_ref())?;
                self.set_status(batch, DiskANNStageStatus::Sealed)?;
            }
            Ok(())
        })?;
        self.repository.open_source(self.generation, control)
    }

    /// Delete at most a bounded record page from an unsealed generation. The final step removes its state; published/sealed reclamation requires a separate catalog lifecycle owner.
    pub fn discard_step(
        &mut self,
        max_records: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        control.check()?;
        if max_records == 0 {
            return Err(invalid("discard requires a positive record limit"));
        }
        self.attempted = true;
        let limit = max_records.min(KEY_PAGE_LIMIT);
        let keys = Keys::new(self.generation);
        let state_key = keys.key(Kind::State);
        let mut complete = false;
        self.repository.mutate(control, &mut |read, batch| {
            let Some(state) = self.state(read, control)? else {
                if read.contains_prefix_budgeted(keys.prefix(), control)? {
                    return Err(invalid("generation has orphaned records"));
                }
                complete = true;
                return Ok(());
            };
            if state.status == DiskANNStageStatus::Sealed {
                return Err(invalid(
                    "sealed generation requires catalog-owned reclamation",
                ));
            }
            let page = key_page(read, keys, Some(state_key.as_ref()), limit, control)?;
            self.fence(batch)?;
            for (key, _) in page.iter() {
                batch.delete(key.as_ref())?;
            }
            complete = page.len() < limit;
            if complete {
                batch.delete(state_key.as_ref())?;
            } else {
                self.set_status(batch, DiskANNStageStatus::Discarding)?;
            }
            Ok(())
        })?;
        Ok(complete)
    }

    fn state(
        &self,
        read: &dyn KeyValueRead,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<State>> {
        let state = load_state(read, self.generation, control)?;
        if state.is_some_and(|state| state.owner != self.owner) {
            return Err(invalid("generation staging owner differs"));
        }
        Ok(state)
    }

    fn require_state(
        &self,
        read: &dyn KeyValueRead,
        control: &StorageReadControl,
    ) -> StorageBackendResult<State> {
        self.state(read, control)?
            .ok_or_else(|| invalid("generation has not been started or was discarded"))
    }

    fn fence(&self, batch: &mut dyn KeyValueBatch) -> StorageBackendResult<()> {
        batch.require_unchanged(&database_key())?;
        batch.require_unchanged(Keys::new(self.generation).key(Kind::State).as_ref())
    }

    fn set_status(
        &self,
        batch: &mut dyn KeyValueBatch,
        status: DiskANNStageStatus,
    ) -> StorageBackendResult<()> {
        batch.put(
            Keys::new(self.generation).key(Kind::State).as_ref(),
            &State {
                status,
                owner: self.owner,
            }
            .encode(),
        )
    }
}

fn load_state(
    read: &dyn KeyValueRead,
    generation: DiskANNGeneration,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<State>> {
    if read_data_identity(read, control)? != Some(generation.database()) {
        return Err(invalid("generation belongs to another data identity"));
    }
    let key = Keys::new(generation).key(Kind::State);
    fixed(control, |visit| {
        read.visit_value_bounded(key.as_ref(), STATE_BYTES, control, visit)
    })?
    .map(State::decode)
    .transpose()
}

fn verify_manifest(
    read: &dyn KeyValueRead,
    keys: Keys,
    expected: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut matches = false;
    let mut visited = false;
    read.visit_value_bounded(
        keys.key(Kind::Record(DiskANNRecordKey::Manifest)).as_ref(),
        expected.len(),
        control,
        &mut |bytes| {
            if visited {
                matches = false;
                return Err(invalid("frozen manifest returned repeatedly"));
            }
            visited = true;
            matches = bytes == Some(expected);
            Ok(())
        },
    )?;
    control.check()?;
    if !visited || !matches {
        return Err(invalid("frozen manifest differs"));
    }
    Ok(())
}

fn verify_streams(
    source: &KeyValueDiskANNSource,
    manifest: &DiskANNManifest,
    maximum: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let stored = read_record(source, DiskANNRecordKey::Manifest, maximum, control)?;
    if DiskANNManifest::decode(manifest.input().generation, &stored, control)? != *manifest {
        return Err(invalid("retained manifest differs"));
    }
    drop(stored);
    let mut sealer = DiskANNArtifactSealer::new(*manifest, control)?;
    let mut after = None;
    loop {
        let page = source.keys_after(
            after.as_ref().map(|key: &super::keys::Key| key.as_ref()),
            control,
        )?;
        let Some((last, _)) = page.last() else { break };
        after = Some(*last);
        for (_, kind) in page.iter() {
            match kind {
                Kind::State | Kind::Record(DiskANNRecordKey::Manifest) => {}
                Kind::Record(key) => {
                    let bytes = read_record(source, *key, maximum, control)?;
                    match key {
                        DiskANNRecordKey::Codebook => sealer.codebook(&bytes)?,
                        DiskANNRecordKey::Codes(first) => sealer.code_batch(*first, &bytes)?,
                        DiskANNRecordKey::Side(first) => sealer.side_batch(*first, &bytes)?,
                        DiskANNRecordKey::Manifest => unreachable!("handled above"),
                    }
                }
                Kind::Graph(id) => {
                    let mut bytes = BudgetedVec::new(control.memory());
                    source.value(*kind, PAGE_BYTES, control, &mut |value| {
                        bytes.extend_from_slice(value)?;
                        Ok(())
                    })?;
                    sealer.graph_page(*id, &bytes)?;
                }
            }
        }
    }
    sealer.finish()?;
    Ok(())
}
