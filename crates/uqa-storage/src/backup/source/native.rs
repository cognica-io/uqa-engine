//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered native codecs share one bounded preparation and retirement boundary.

use super::{
    BackupCut, BackupImageSource, BackupResult, BackupSourceStatistics, BackupSourceStep,
    BackupSourceUnit, UnitAddress, UnitKind,
};
use crate::{
    backup::{BackupError, NamespaceId},
    image_io::ImageReader,
    read_control::StorageReadControl,
};
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
    fn from(unit: NativeImageUnit) -> Self {
        Self {
            namespace: unit.namespace,
            generation: unit.generation,
            family: unit.family,
            key: unit.key,
            kind: unit.kind,
            reader: Some(unit.reader),
            ready: false,
        }
    }
}

struct Active<S> {
    entries: S,
    current: Option<Current>,
}

enum State<S> {
    Active(Active<S>),
    Complete,
    Failed,
}

/// Stream a complete native mapping with bounded selection/preparation, unknown unit lengths and one current codec. The source owns admission and retirement: invalid calls, errors and unwinds retire the cut and cannot resume. Successful EOF retires each codec before the next component is selected.
pub struct NativeBackupImage<S> {
    cut: BackupCut,
    state: State<S>,
    control: StorageReadControl,
    statistics: BackupSourceStatistics,
}

impl<S: NativeImageEntries> NativeBackupImage<S> {
    pub fn new(cut: BackupCut, entries: S, control: &StorageReadControl) -> BackupResult<Self> {
        control.check()?;
        Ok(Self {
            cut,
            state: State::Active(Active {
                entries,
                current: None,
            }),
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
}

impl<S: NativeImageEntries> BackupImageSource for NativeBackupImage<S> {
    fn cut(&self) -> BackupCut {
        self.cut
    }

    fn advance(&mut self, maximum_positions: usize) -> BackupResult<BackupSourceStep> {
        let mut active = match std::mem::replace(&mut self.state, State::Failed) {
            State::Active(active) => active,
            State::Complete => {
                self.state = State::Complete;
                return Ok(BackupSourceStep::Complete);
            }
            State::Failed => return Err(BackupError::Finished),
        };
        self.control.check()?;
        active.entries.check()?;
        if maximum_positions == 0 {
            return Err(BackupError::Invalid("zero native image scan window"));
        }
        if let Some(current) = active.current.as_mut().filter(|unit| unit.reader.is_some()) {
            if current.ready {
                return Err(BackupError::Invalid("native image unit is not consumed"));
            }
            let step = self.prepare(current, maximum_positions)?;
            self.control.check()?;
            active.entries.check()?;
            self.state = State::Active(active);
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
                self.state = State::Complete;
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
                let step = self.prepare(&mut current, maximum_positions - 1)?;
                active.current = Some(current);
                step
            }
        };
        self.control.check()?;
        active.entries.check()?;
        self.state = State::Active(active);
        Ok(step)
    }

    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        let State::Active(active) = &self.state else {
            return None;
        };
        let current = active.current.as_ref().filter(|unit| unit.ready)?;
        Some(BackupSourceUnit {
            address: current.address(),
            kind: current.kind,
            decoded_bytes: None,
        })
    }

    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let mut active = match std::mem::replace(&mut self.state, State::Failed) {
            State::Active(active) => active,
            State::Complete => {
                self.state = State::Complete;
                return Err(BackupError::Finished);
            }
            State::Failed => return Err(BackupError::Finished),
        };
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
        if count == 0 && !output.is_empty() {
            current.reader = None;
        }
        self.state = State::Active(active);
        Ok(count)
    }

    fn statistics(&self) -> BackupSourceStatistics {
        self.statistics
    }
}

#[cfg(test)]
mod tests;
