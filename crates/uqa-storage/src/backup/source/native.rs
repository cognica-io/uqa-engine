//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered native codecs share one bounded preparation and retirement boundary.

use super::{
    BackupCut, BackupImageSource, BackupResult, BackupSourceStatistics, BackupSourceStep,
    BackupSourceUnit, SourceUnitHash, UnitAddress, UnitFingerprint, UnitKind,
};
use crate::{
    backup::{BackupError, NamespaceId},
    image_io::ImageReader,
    read_control::StorageReadControl,
};
use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

/// One admitted native component. Its immutable source and payload reservations stay with the reader; its key is already admitted by the mapping owner. No complete encoded payload is retained here.
pub struct NativeImageUnit {
    pub namespace: NamespaceId,
    pub generation: u64,
    pub family: u16,
    pub key: BudgetedVec<u8>,
    pub kind: UnitKind,
    pub reader: ImageReader<'static>,
}

/// Selecting a component performs at most one mapping-defined position. Pending means that position contains no unit. Preparation of the selected codec is scheduled separately.
pub enum NativeImageStep {
    Unit(NativeImageUnit),
    Pending,
    Complete,
}

/// A retained native generation supplies component membership and their existing codecs. Selection must not encode, copy or validate the complete component, inspect a live replacement, or retain a mutable guard across calls.
pub trait NativeImageEntries: Send {
    /// Observe retirement or failure of the retained generation during every selection and output call.
    fn check(&self) -> BackupResult<()>;

    /// An ownership group must cover every immutable input of this family. Unsupplied groups retain the complete streaming comparison.
    fn fingerprint_generation(
        &self,
        _family: u16,
    ) -> Option<&crate::memory_capture::MemoryImageGeneration> {
        None
    }
    fn next_image(&mut self, control: &StorageReadControl) -> BackupResult<NativeImageStep>;
}

struct Current {
    namespace: NamespaceId,
    generation: u64,
    family: u16,
    key: BudgetedVec<u8>,
    kind: UnitKind,
    reader: Option<ImageReader<'static>>,
    ready: bool,
    offered: bool,
    read_started: bool,
    cache_key: Option<[u8; 32]>,
    immutable_owner: Option<crate::memory_capture::MemoryImageOwner>,
    fingerprint: Option<UnitFingerprint>,
    hash: Option<Sha256>,
    hashed: u64,
    completed: Option<UnitFingerprint>,
}

impl Current {
    fn address(&self) -> UnitAddress<'_> {
        UnitAddress {
            namespace: self.namespace,
            generation: self.generation,
            family: self.family,
            key: &self.key,
        }
    }
}

impl From<NativeImageUnit> for Current {
    fn from(mut unit: NativeImageUnit) -> Self {
        let immutable_owner = unit.reader.take_image_owner();
        Self {
            namespace: unit.namespace,
            generation: unit.generation,
            family: unit.family,
            key: unit.key,
            kind: unit.kind,
            reader: Some(unit.reader),
            ready: false,
            offered: false,
            read_started: false,
            cache_key: None,
            immutable_owner,
            fingerprint: None,
            hash: None,
            hashed: 0,
            completed: None,
        }
    }
}

struct Active<S> {
    entries: S,
    current: Option<Current>,
}

/// Stream a complete native mapping with bounded selection/preparation, unknown unit lengths and one current codec. The source owns admission and retirement: invalid calls, errors and unwinds retire the cut and cannot resume. Successful EOF retires each codec before the next component is selected.
pub struct NativeBackupImage<S> {
    cut: BackupCut,
    active: Option<Active<S>>,
    complete: bool,
    control: StorageReadControl,
    statistics: BackupSourceStatistics,
}

impl<S: NativeImageEntries> NativeBackupImage<S> {
    pub fn new(cut: BackupCut, entries: S, control: &StorageReadControl) -> BackupResult<Self> {
        control.check()?;
        Ok(Self {
            cut,
            active: Some(Active {
                entries,
                current: None,
            }),
            complete: false,
            control: control.clone(),
            statistics: BackupSourceStatistics::default(),
        })
    }

    fn add_positions(&mut self, count: usize) -> BackupResult<()> {
        self.statistics.visited_positions = self
            .statistics
            .visited_positions
            .checked_add(count as u64)
            .ok_or(BackupError::Invalid("native image position overflow"))?;
        Ok(())
    }

    fn prepare(&mut self, current: &mut Current, maximum: usize) -> BackupResult<BackupSourceStep> {
        if maximum == 0 {
            return Ok(BackupSourceStep::Pending);
        }
        let progress = current.reader.as_mut().unwrap().prepare(maximum)?;
        self.add_positions(progress.positions)?;
        current.ready = progress.ready;
        Ok(if progress.ready {
            BackupSourceStep::Unit
        } else {
            BackupSourceStep::Pending
        })
    }

    fn select(
        &mut self,
        maximum_positions: usize,
        comparison: bool,
    ) -> BackupResult<BackupSourceStep> {
        if self.complete {
            return Ok(BackupSourceStep::Complete);
        }
        let mut active = self.active.take().ok_or(BackupError::Finished)?;
        self.control.check()?;
        active.entries.check()?;
        if maximum_positions == 0 {
            return Err(BackupError::Invalid("zero native image scan window"));
        }
        if let Some(current) = active.current.as_mut().filter(|unit| unit.reader.is_some()) {
            if current.ready || current.offered {
                return Err(BackupError::Invalid("native image unit is not consumed"));
            }
            let step = self.prepare(current, maximum_positions)?;
            self.control.check()?;
            active.entries.check()?;
            self.active = Some(active);
            return Ok(step);
        }
        if let Some(previous) = &mut active.current {
            previous.ready = false;
        }
        let step = active.entries.next_image(&self.control)?;
        self.add_positions(1)?;
        self.control.check()?;
        active.entries.check()?;
        let step = match step {
            NativeImageStep::Complete => {
                self.complete = true;
                return Ok(BackupSourceStep::Complete);
            }
            NativeImageStep::Pending => BackupSourceStep::Pending,
            NativeImageStep::Unit(unit) => {
                let mut current = Current::from(unit);
                if current.family == 0
                    || active
                        .current
                        .as_ref()
                        .is_some_and(|previous| previous.address() >= current.address())
                {
                    return Err(BackupError::Invalid("unordered native image units"));
                }
                if let Some(generation) = active.entries.fingerprint_generation(current.family) {
                    let key = crate::memory_capture::MemoryImageGeneration::address_key(
                        self.cut,
                        current.address(),
                        current.kind,
                    );
                    current.fingerprint = generation.get(&key, current.immutable_owner.as_ref());
                    current.cache_key = Some(key);
                    if comparison {
                        current.hash = Some(Sha256::new());
                    }
                    if current.fingerprint.is_some() {
                        self.statistics.metadata_read_bytes =
                            self.statistics.metadata_read_bytes.checked_add(40).ok_or(
                                BackupError::Invalid("native fingerprint metadata overflow"),
                            )?;
                    }
                }
                let step = if comparison && current.fingerprint.is_some() {
                    current.offered = true;
                    BackupSourceStep::Unit
                } else {
                    self.prepare(&mut current, maximum_positions - 1)?
                };
                active.current = Some(current);
                step
            }
        };
        self.control.check()?;
        active.entries.check()?;
        self.active = Some(active);
        Ok(step)
    }
}
impl<S: NativeImageEntries> BackupImageSource for NativeBackupImage<S> {
    fn cut(&self) -> BackupCut {
        self.cut
    }
    fn advance(&mut self, maximum_positions: usize) -> BackupResult<BackupSourceStep> {
        self.select(maximum_positions, false)
    }
    fn advance_for_comparison(
        &mut self,
        maximum_positions: usize,
    ) -> BackupResult<BackupSourceStep> {
        self.select(maximum_positions, true)
    }

    fn prepare_current(&mut self, maximum_positions: usize) -> BackupResult<bool> {
        let mut active = self.active.take().ok_or(BackupError::Finished)?;
        self.control.check()?;
        active.entries.check()?;
        let current = active.current.as_mut().ok_or(BackupError::Finished)?;
        if !current.ready {
            if !current.offered {
                return Err(BackupError::Invalid("native image unit is not selected"));
            }
            self.prepare(current, maximum_positions)?;
        }
        let ready = current.ready;
        self.control.check()?;
        active.entries.check()?;
        self.active = Some(active);
        Ok(ready)
    }

    fn reuse_current(&mut self, parent: UnitFingerprint) -> BackupResult<bool> {
        let mut active = self.active.take().ok_or(BackupError::Finished)?;
        self.control.check()?;
        active.entries.check()?;
        let current = active
            .current
            .as_mut()
            .filter(|unit| unit.offered || unit.ready)
            .ok_or(BackupError::Finished)?;
        let reused = !current.read_started
            && current.reader.is_some()
            && current.fingerprint == Some(parent);
        if reused {
            current.reader = None;
            current.hash = None;
            current.completed = Some(parent);
            current.ready = true;
            current.offered = false;
        }
        self.active = Some(active);
        Ok(reused)
    }

    fn current_hash(&self) -> SourceUnitHash {
        let Some(unit) = self
            .active
            .as_ref()
            .and_then(|active| active.current.as_ref())
        else {
            return SourceUnitHash::Repository;
        };
        if let Some(fingerprint) = unit.completed {
            SourceUnitHash::Complete(fingerprint)
        } else if unit.hash.is_some() {
            SourceUnitHash::Computing
        } else {
            SourceUnitHash::Repository
        }
    }

    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        let active = self.active.as_ref()?;
        let current = active
            .current
            .as_ref()
            .filter(|unit| unit.ready || unit.offered)?;
        Some(BackupSourceUnit {
            address: current.address(),
            kind: current.kind,
            decoded_bytes: current.fingerprint.map(|fp| fp.decoded_bytes),
        })
    }

    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let mut active = self.active.take().ok_or(BackupError::Finished)?;
        self.control.check()?;
        active.entries.check()?;
        let current = active
            .current
            .as_mut()
            .filter(|unit| unit.ready)
            .ok_or(BackupError::Invalid("no prepared native image unit"))?;
        let count = match &mut current.reader {
            Some(reader) => reader.read(output)?,
            None => 0,
        };
        self.control.check()?;
        active.entries.check()?;
        self.statistics.copied_payload_bytes = self
            .statistics
            .copied_payload_bytes
            .checked_add(count as u64)
            .ok_or(BackupError::Invalid("native image byte count overflow"))?;
        current.read_started |= count != 0;
        if let Some(hash) = &mut current.hash {
            hash.update(&output[..count]);
            current.hashed = current
                .hashed
                .checked_add(count as u64)
                .ok_or(BackupError::Integrity)?;
        }
        if count == 0 && !output.is_empty() {
            current.reader = None;
            if let Some(hash) = current.hash.take() {
                let fingerprint = UnitFingerprint {
                    decoded_bytes: current.hashed,
                    digest: crate::backup::ImageDigest::from_bytes(hash.finalize().into()),
                };
                let generation = active
                    .entries
                    .fingerprint_generation(current.family)
                    .expect("hash belongs to a source generation");
                generation.observe(
                    current.cache_key.expect("hash owns cache key"),
                    current.immutable_owner.as_ref(),
                    fingerprint,
                );
                current.completed = Some(fingerprint);
            }
        }
        self.active = Some(active);
        Ok(count)
    }

    fn statistics(&self) -> BackupSourceStatistics {
        self.statistics
    }
}

#[cfg(test)]
mod tests;
