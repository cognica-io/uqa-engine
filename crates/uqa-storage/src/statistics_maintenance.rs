//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable statistics-maintenance state and evaluated concurrent counter changes.

use crate::{CatalogFacade, StorageBackendResult};
use serde::{Deserialize, Serialize};

const MAX_DIRTY_AGE_MS: u64 = 60_000;
/// What a session may keep to itself of a small table, whose analysis threshold is fifty changes.
const MIN_KEPT_CHANGES: u64 = 16;

/// The changes that make an analysis due: the analyze threshold of 50 rows and the scale factor of a tenth of the analyzed rows that `PostgreSQL` uses.
const fn change_threshold(analyzed_rows: u64) -> u64 {
    50 + analyzed_rows / 10
}

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
    /// The commit sequence the last analysis sampled at, where its provider numbers commits: every commit up to it is in the statistics. A record without it says nothing about what its analysis saw.
    #[serde(skip_serializing_if = "Option::is_none")]
    analyzed_at: Option<u64>,
    /// Identity of the last analysis, independent of later counter increments.
    /// Legacy records acquire it on their next completed analysis.
    #[serde(skip_serializing_if = "Option::is_none")]
    analysis_generation: Option<u64>,
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
                    || self.changes >= change_threshold(self.analyzed_rows.unwrap_or(0))
                    || now.saturating_sub(self.dirty_since_ms) >= MAX_DIRTY_AGE_MS))
    }

    /// Whether a session may keep `unrecorded` changes of its commits to itself instead of recording them now. A record written by every commit is a write of two records to each of them, and the count decides only when an analysis becomes due, so a session records the changes that decide something: the first one after an analysis, which marks the statistics stale and starts their age, and the ones that make an analysis due. Otherwise it keeps up to a quarter of what makes one due, or sixteen changes of a small table, which bounds what sessions hide from one another and what a session loses when it ends. While an analysis is due already, more changes decide nothing.
    pub fn defers(&self, unrecorded: u64, now: u64) -> bool {
        if !self.dirty() {
            return false;
        }
        let Some(analyzed_rows) = self.analyzed_rows else {
            // Changes without analyzed rows are those of a table without statistics, whose analysis is due whatever it counts.
            return true;
        };
        let threshold = change_threshold(analyzed_rows);
        if analyzed_rows == 0
            || self.changes >= threshold
            || now.saturating_sub(self.dirty_since_ms) >= MAX_DIRTY_AGE_MS
        {
            return true;
        }
        self.changes.saturating_add(unrecorded) < threshold
            && unrecorded < (threshold / 4).max(MIN_KEPT_CHANGES)
    }

    pub fn analyzed_for(
        catalog: &dyn CatalogFacade,
        table: &str,
        object_id: [u8; 16],
        rows: u64,
        statistics_format: u32,
        sampled_at: Option<u64>,
    ) -> StorageBackendResult<()> {
        let mut state = Self::load_for(catalog, table, object_id)?;
        state.analyzed(object_id, rows, statistics_format, sampled_at)?;
        state.save(catalog, table)
    }

    /// Record a completed analysis of `rows` rows, which covers every change counted before it. `sampled_at` is the commit sequence its sample read at, or `None` when that is not known.
    pub fn analyzed(
        &mut self,
        object_id: [u8; 16],
        rows: u64,
        statistics_format: u32,
        sampled_at: Option<u64>,
    ) -> StorageBackendResult<()> {
        self.object_id = Some(object_id);
        self.advance_generation()?;
        self.analysis_generation = Some(self.generation);
        self.changes = 0;
        self.dirty_since_ms = 0;
        self.analyzed_rows = Some(rows);
        self.statistics_format = statistics_format;
        self.analyzed_at = sampled_at;
        Ok(())
    }

    /// Whether the last analysis sampled every commit up to `through`, so that changes a session kept of commits up to there need no counting.
    pub fn covers(&self, through: Option<u64>) -> bool {
        matches!((self.analyzed_at, through), (Some(at), Some(through)) if through <= at)
    }

    /// Row changes retain the last statistics publication; another analysis
    /// replaces it even when the resulting estimates happen to be identical.
    pub fn same_analysis(&self, other: &Self) -> bool {
        self.analysis_generation == other.analysis_generation
            && self.analyzed_at == other.analyzed_at
            && self.analyzed_rows == other.analyzed_rows
            && self.statistics_format == other.statistics_format
    }

    /// Publish this sample's reset over changes recorded since `before`. The caller
    /// must first verify that the relation and its last statistics publication
    /// still have the captured identities. A changed data generation can also
    /// represent writes retained privately by their sessions: keep one pending
    /// change in that case so their absence from the counter cannot mark the
    /// sampled statistics current. The counter is a maintenance estimate.
    pub fn merge_sample(
        &self,
        before: &Self,
        current: &Self,
        data_changed: bool,
        now: u64,
    ) -> StorageBackendResult<Option<Self>> {
        let Some(mut merged) = Self::merge(before, self, current)
            .map_err(crate::mvcc::VersionError::into_storage_error)?
        else {
            return Ok(None);
        };
        if data_changed && !merged.dirty() {
            merged.changes = 1;
            merged.dirty_since_ms = now;
        }
        Ok(Some(merged))
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
            // An analysis of the merged transaction is the one whose statistics it publishes.
            analyzed_at: if after.analyzed_at == before.analyzed_at {
                current.analyzed_at
            } else {
                after.analyzed_at
            },
            analysis_generation: if after.analysis_generation == before.analysis_generation {
                current.analysis_generation
            } else {
                after.analysis_generation
            },
        }))
    }
}
