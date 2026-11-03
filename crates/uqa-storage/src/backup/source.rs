//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{BackupCut, BackupResult, UnitAddress, UnitKind};

pub struct BackupSourceUnit<'a> {
    pub address: UnitAddress<'a>,
    pub kind: UnitKind,
    pub decoded_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupSourceStep {
    Unit,
    Pending,
    Complete,
}

/// Explicit source work. These counters do not estimate physical page or cache activity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackupSourceStatistics {
    pub visited_positions: u64,
    pub copied_payload_bytes: u64,
    pub metadata_read_bytes: u64,
    pub excluded_units: u64,
}

/// One provider-owned complete image over a retained cut. Units must have strictly increasing addresses and exact declared lengths. Each call releases source physical reads before returning; dropping the source releases its cut. Implementing this contract does not qualify another provider or mapping.
pub trait BackupImageSource {
    fn cut(&self) -> BackupCut;
    fn advance(&mut self, maximum_positions: usize) -> BackupResult<BackupSourceStep>;
    fn current_unit(&self) -> Option<BackupSourceUnit<'_>>;
    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize>;
    fn statistics(&self) -> BackupSourceStatistics;
}
