//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Three retained cluster views yield per-document changes, never a union of stale whole clusters.

use super::{PreparedRecordWrite, Resolver, VersionError, VersionResult};
use crate::clustered_postings::{
    decode_occurrence_cluster_budgeted, encode_occurrence_cluster_controlled, OccurrencePosting,
};
use crate::read_control::StorageReadControl;
use uqa_core::memory::{Budgeted, BudgetedVec};

fn decode(
    cluster: u64,
    score: Option<&[u8]>,
    positions: Option<&[u8]>,
    control: &StorageReadControl,
) -> VersionResult<Budgeted<Vec<OccurrencePosting>>> {
    match (score, positions) {
        (None, None) => Ok(Budgeted::new(
            Vec::new(),
            control.memory().empty_reservation(),
        )),
        (Some(score), Some(positions)) => Ok(decode_occurrence_cluster_budgeted(
            cluster,
            score,
            positions,
            control.memory(),
            || {
                control.cancellation().check()?;
                Ok(())
            },
        )?),
        _ => Err(VersionError::InvalidEncoding(
            "incomplete occurrence cluster pair",
        )),
    }
}

impl Resolver<'_> {
    pub(super) fn merge_cluster(
        &self,
        mutation: usize,
        score: &PreparedRecordWrite,
        positions: &PreparedRecordWrite,
        cluster: u64,
    ) -> VersionResult<()> {
        let base_score = self.base.get(score.key(), self.control)?;
        let base_positions = self.base.get(positions.key(), self.control)?;
        let current_score = self.current.get(score.key(), self.control)?;
        let current_positions = self.current.get(positions.key(), self.control)?;
        let base = decode(
            cluster,
            super::bytes(base_score.as_ref()),
            super::bytes(base_positions.as_ref()),
            self.control,
        )?;
        let evaluated = decode(cluster, score.value(), positions.value(), self.control)?;
        let current = decode(
            cluster,
            super::bytes(current_score.as_ref()),
            super::bytes(current_positions.as_ref()),
            self.control,
        )?;
        let mut merged = BudgetedVec::new(self.control.memory());
        merged.reserve(
            current
                .len()
                .checked_add(evaluated.len())
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?,
        )?;
        let mut base = base.iter().peekable();
        let mut evaluated = evaluated.iter().peekable();
        let mut current = current.iter().peekable();
        loop {
            self.control.cancellation().check()?;
            let Some(id) = [base.peek(), evaluated.peek(), current.peek()]
                .into_iter()
                .flatten()
                .map(|entry| entry.doc_id)
                .min()
            else {
                break;
            };
            let before = base.next_if(|entry| entry.doc_id == id);
            let after = evaluated.next_if(|entry| entry.doc_id == id);
            let latest = current.next_if(|entry| entry.doc_id == id);
            let chosen = if before == after {
                latest
            } else {
                if before != latest {
                    return Err(VersionError::WriteConflict {
                        mutation,
                        expected: score.expected(),
                        actual: super::revision(self.current, score.key(), self.control)?,
                    });
                }
                after
            };
            if let Some(entry) = chosen {
                merged.push(entry)?;
            }
        }
        if merged.is_empty() {
            self.replace(score.key(), None)?;
            self.replace(positions.key(), None)
        } else {
            let (scores, positions_blob) =
                encode_occurrence_cluster_controlled(merged.iter().copied(), self.control)?;
            self.replace(score.key(), Some(&scores))?;
            self.replace(positions.key(), Some(&positions_blob))
        }
    }
}
