//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable statistics-maintenance state and evaluated concurrent counter changes.

use crate::{CatalogFacade, StorageBackendResult};
use serde::{Deserialize, Serialize};

const MAX_DIRTY_AGE_MS: u64 = 60_000;

#[cfg(test)]
mod tests;

/// Transactional maintenance metadata. Existing column statistics remain
/// available while this record tracks changes awaiting a replacement.
#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatisticsMaintenance {
    object_id: Option<[u8; 16]>,
    generation: u64,
    changes: u64,
    dirty_since_ms: u64,
    analyzed_rows: Option<u64>,
    statistics_format: u32,
}

impl StatisticsMaintenance {
    pub const KEY_PREFIX: &'static str = "uqa.statistics.maintenance.v1:";

    pub fn key(table: &str) -> String {
        format!("{}{table}", Self::KEY_PREFIX)
    }

    pub fn load(catalog: &dyn CatalogFacade, table: &str) -> StorageBackendResult<Self> {
        catalog
            .get_metadata(&Self::key(table))?
            .map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub fn load_for(
        catalog: &dyn CatalogFacade,
        table: &str,
        object_id: [u8; 16],
    ) -> StorageBackendResult<Self> {
        let state = Self::load(catalog, table)?;
        Ok(
            if state.object_id.is_some_and(|stored| stored != object_id) {
                Self::default()
            } else {
                state
            },
        )
    }

    pub fn save(&self, catalog: &dyn CatalogFacade, table: &str) -> StorageBackendResult<()> {
        catalog.save_statistics_maintenance(table, self)
    }

    pub fn dirty(&self) -> bool {
        self.changes != 0
    }

    pub fn missing(&self, statistics_empty: bool) -> bool {
        self.analyzed_rows.is_none() && statistics_empty
    }

    pub fn invalidates_existing_statistics(&self) -> bool {
        // Initial imported statistics predate this maintenance protocol.
        // A new write adopts that baseline before marking it stale.
        self.dirty() && self.analyzed_rows.is_some()
    }

    pub fn due(&self, missing: bool, now: u64, statistics_format: u32) -> bool {
        self.statistics_format != statistics_format
            || missing
            || (self.dirty()
                && (self.analyzed_rows == Some(0)
                    || self.changes >= 50 + self.analyzed_rows.unwrap_or(0) / 10
                    || now.saturating_sub(self.dirty_since_ms) >= MAX_DIRTY_AGE_MS))
    }

    pub fn analyzed_for(
        catalog: &dyn CatalogFacade,
        table: &str,
        object_id: [u8; 16],
        rows: u64,
        statistics_format: u32,
    ) -> StorageBackendResult<()> {
        let mut state = Self::load_for(catalog, table, object_id)?;
        state.object_id = Some(object_id);
        state.advance_generation()?;
        state.changes = 0;
        state.dirty_since_ms = 0;
        state.analyzed_rows = Some(rows);
        state.statistics_format = statistics_format;
        state.save(catalog, table)
    }

    fn advance_generation(&mut self) -> StorageBackendResult<()> {
        self.generation = self.generation.checked_add(1).ok_or_else(|| {
            crate::StorageBackendError::Other("statistics generation space exhausted".into())
        })?;
        Ok(())
    }

    pub fn record_changes(
        &mut self,
        object_id: [u8; 16],
        count: u64,
        previous_rows: Option<u64>,
        now: u64,
    ) -> StorageBackendResult<()> {
        self.object_id = Some(object_id);
        self.advance_generation()?;
        if self.analyzed_rows.is_none() {
            self.analyzed_rows = previous_rows;
        }
        if !self.dirty() {
            self.dirty_since_ms = now;
        }
        self.changes = self.changes.checked_add(count).ok_or_else(|| {
            crate::StorageBackendError::Other("statistics change count overflow".into())
        })?;
        Ok(())
    }

    /// Encode the fixed-size state under the same budget and cancellation control as its record.
    pub fn encode(
        &self,
        control: &crate::read_control::StorageReadControl,
    ) -> crate::mvcc::VersionResult<uqa_core::memory::BudgetedVec<u8>> {
        crate::key_value::record_json::encode(self, control)
    }

    /// Merge evaluated counter changes. A replacement object or overlapping reset that cannot retain the original baseline remains a conditional canonical replacement.
    pub(crate) fn merge(
        before: &Self,
        after: &Self,
        current: &Self,
    ) -> crate::mvcc::VersionResult<Option<Self>> {
        use crate::mvcc::VersionError;
        if before
            .object_id
            .is_some_and(|id| Some(id) != after.object_id)
            || current
                .object_id
                .is_some_and(|id| Some(id) != after.object_id)
            || after.generation < before.generation
        {
            return Ok(None);
        }
        let changes =
            i128::from(current.changes) + i128::from(after.changes) - i128::from(before.changes);
        if changes < 0 {
            return Ok(None);
        }
        let changes = u64::try_from(changes)
            .map_err(|_| VersionError::InvalidEncoding("statistics change count overflow"))?;
        let generation = current
            .generation
            .checked_add(after.generation - before.generation)
            .ok_or(VersionError::InvalidEncoding(
                "statistics generation space exhausted",
            ))?;
        let changed_estimate = after.statistics_format != 0
            && (after.analyzed_rows != before.analyzed_rows
                || after.statistics_format != before.statistics_format);
        Ok(Some(Self {
            object_id: after.object_id,
            generation,
            changes,
            dirty_since_ms: if changes == 0 {
                0
            } else {
                [after.dirty_since_ms, current.dirty_since_ms]
                    .into_iter()
                    .filter(|time| *time != 0)
                    .min()
                    .unwrap_or(0)
            },
            analyzed_rows: if changed_estimate {
                after.analyzed_rows
            } else {
                current.analyzed_rows.or(after.analyzed_rows)
            },
            statistics_format: if changed_estimate {
                after.statistics_format
            } else {
                current.statistics_format
            },
        }))
    }
}
