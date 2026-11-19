//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! An admitted provider image retains its concrete owner behind the common stream interface.

use super::{
    BackupCut, BackupImageSource, BackupResult, BackupSourceStatistics, BackupSourceStep,
    BackupSourceUnit,
};
use crate::read_control::StorageReadControl;
use uqa_core::memory::MemoryReservation;

/// An independently owned, movable provider image. Its concrete source and cut outlive the provider or Engine that captured them. The source enforces its original cancellation and streaming lifecycle; this adapter preserves the format, addresses, bytes and actual-work counters without opening another source database.
pub struct OwnedBackupImage {
    source: Box<dyn BackupImageSource + Send>,
    // Drop the concrete owner before releasing its heap allowance.
    _memory: MemoryReservation,
}

impl OwnedBackupImage {
    /// Admit the concrete source header before invoking its capture boundary. Failed construction drops any partially retained source; payload and cursor allowances remain with their existing owners.
    pub fn capture<S: BackupImageSource + Send + 'static>(
        control: &StorageReadControl,
        capture: impl FnOnce() -> BackupResult<S>,
    ) -> BackupResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<S>())?;
        let source = capture()?;
        control.check()?;
        Ok(Self {
            source: Box::new(source),
            _memory: memory,
        })
    }
}

impl BackupImageSource for OwnedBackupImage {
    fn cut(&self) -> BackupCut {
        self.source.cut()
    }

    fn advance(&mut self, maximum_positions: usize) -> BackupResult<BackupSourceStep> {
        self.source.advance(maximum_positions)
    }

    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        self.source.current_unit()
    }

    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        self.source.read_current(output)
    }

    fn statistics(&self) -> BackupSourceStatistics {
        self.source.statistics()
    }
}

#[cfg(test)]
mod tests;
