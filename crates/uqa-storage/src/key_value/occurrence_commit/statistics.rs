//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checked field-count deltas retain the analyzer revision selected during evaluation.

use super::{PreparedRecordWrite, Resolver, VersionError, VersionResult};
use crate::key_value::inverted_index::FieldStats;

impl Resolver<'_> {
    pub(super) fn merge_statistics(
        &self,
        mutation: usize,
        write: &PreparedRecordWrite,
    ) -> VersionResult<()> {
        let base = self.base.get(write.key(), self.control)?;
        let current = self.current.get(write.key(), self.control)?;
        let before = base
            .as_ref()
            .and_then(|row| row.value())
            .map(|value| FieldStats::from_bytes(value))
            .transpose()?;
        let after = write.value().map(FieldStats::from_bytes).transpose()?;
        let latest = current
            .as_ref()
            .and_then(|row| row.value())
            .map(|value| FieldStats::from_bytes(value))
            .transpose()?;
        let mut revisions = [before, after, latest]
            .into_iter()
            .flatten()
            .map(|stats| stats.revision);
        let Some(revision) = revisions.next() else {
            return self.replace(write.key(), None);
        };
        if revisions.any(|candidate| candidate != revision) {
            return Err(VersionError::WriteConflict {
                mutation,
                expected: write.expected(),
                actual: super::revision(self.current, write.key(), self.control)?,
            });
        }
        let delta = |select: fn(FieldStats) -> u64| -> VersionResult<u64> {
            let total = i128::from(latest.map_or(0, select)) + i128::from(after.map_or(0, select))
                - i128::from(before.map_or(0, select));
            u64::try_from(total).map_err(|_| {
                VersionError::InvalidEncoding("occurrence field total overflow or underflow")
            })
        };
        let merged = FieldStats {
            revision,
            doc_count: delta(|stats| stats.doc_count)?,
            total_length: delta(|stats| stats.total_length)?,
        };
        if merged.doc_count == 0 {
            if merged.total_length != 0 {
                return Err(VersionError::InvalidEncoding(
                    "empty occurrence field retains length",
                ));
            }
            self.replace(write.key(), None)
        } else {
            self.replace(write.key(), Some(&merged.to_bytes()?))
        }
    }
}
