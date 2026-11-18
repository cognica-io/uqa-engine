//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{BackupCut, BackupResult, ImageDigest, UnitAddress, UnitKind};

mod native;
pub use native::{NativeBackupImage, NativeImageEntries, NativeImageStep, NativeImageUnit};

mod owned;
pub use owned::OwnedBackupImage;

pub struct BackupSourceUnit<'a> {
    pub address: UnitAddress<'a>,
    pub kind: UnitKind,
    /// Exact encoded length when the owner already knows it. A streaming codec may return `None`; capture establishes the exact inventory length from successful EOF without pre-encoding the unit.
    pub decoded_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupSourceStep {
    Unit,
    Pending,
    Complete,
}

/// SHA-256 and exact length of one encoded image unit selected by the provider's current cut, address and mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnitFingerprint {
    pub digest: ImageDigest,
    pub decoded_bytes: u64,
}

/// Sources that maintain their own immutable certificate can hash the ordinary payload stream once and give its completed digest to the repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceUnitHash {
    Repository,
    Computing,
    Complete(UnitFingerprint),
}

/// Explicit source work. These counters do not estimate physical page or cache activity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackupSourceStatistics {
    pub visited_positions: u64,
    pub copied_payload_bytes: u64,
    pub metadata_read_bytes: u64,
    pub excluded_units: u64,
}

impl BackupSourceStatistics {
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            visited_positions: self
                .visited_positions
                .saturating_add(other.visited_positions),
            copied_payload_bytes: self
                .copied_payload_bytes
                .saturating_add(other.copied_payload_bytes),
            metadata_read_bytes: self
                .metadata_read_bytes
                .saturating_add(other.metadata_read_bytes),
            excluded_units: self.excluded_units.saturating_add(other.excluded_units),
        }
    }
}

/// One provider-owned complete image over a retained cut. Units must have strictly increasing addresses; supplied lengths are exact, and an omitted length is established by draining the unit. A nonempty read returns zero only at successful EOF. Each call releases source physical reads before returning; dropping the source releases its cut. Implementing this contract does not qualify another provider or mapping.
pub trait BackupImageSource {
    fn cut(&self) -> BackupCut;

    /// Open the stored view queries on this exact image cut when its mapping exposes physical catalog rows.
    fn stored_view_queries(
        &self,
        limits: crate::catalog::input::CatalogInputLimits,
        control: &crate::read_control::StorageReadControl,
    ) -> BackupResult<Option<super::BackupViewQueryInput>> {
        control.check()?;
        limits.validate()?;
        Ok(None)
    }

    /// Select the raw UTF-8 values of catalog metadata keys with a logical name prefix on this exact committed image cut.
    fn stored_metadata_values(
        &self,
        prefix: &str,
        limits: crate::catalog::input::CatalogInputLimits,
        control: &crate::read_control::StorageReadControl,
    ) -> BackupResult<Option<crate::backup::BackupCatalogTextInput>> {
        limits.validate()?;
        control.check_value_size(prefix.len(), limits.maximum_key_bytes)?;
        Ok(None)
    }

    fn advance(&mut self, maximum_positions: usize) -> BackupResult<BackupSourceStep>;
    /// Offer a certified unit before preparing its payload. Repository callers drive payload preparation only when comparison requires reading it.
    fn advance_for_comparison(
        &mut self,
        maximum_positions: usize,
    ) -> BackupResult<BackupSourceStep> {
        self.advance(maximum_positions)
    }
    fn prepare_current(&mut self, _maximum_positions: usize) -> BackupResult<bool> {
        Ok(true)
    }
    fn current_hash(&self) -> SourceUnitHash {
        SourceUnitHash::Repository
    }
    fn current_unit(&self) -> Option<BackupSourceUnit<'_>>;
    /// Consume the selected unit on an exact certified match. An unavailable or differing fingerprint leaves the payload at its original position for bounded comparison.
    fn reuse_current(&mut self, _parent: UnitFingerprint) -> BackupResult<bool> {
        Ok(false)
    }
    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize>;
    /// A producer that must finish other bounded source work before emitting bytes may return None. A successful zero-byte read still certifies exact unit EOF.
    fn read_current_bounded(
        &mut self,
        output: &mut [u8],
        _maximum_positions: usize,
    ) -> BackupResult<Option<usize>> {
        self.read_current(output).map(Some)
    }
    fn statistics(&self) -> BackupSourceStatistics;
}
