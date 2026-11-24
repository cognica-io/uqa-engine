//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-local index observations shared by the cardinality and cost estimators.

use crate::query_optimizer::IndexScanCandidate;
use uqa_core::{IndexStats, Predicate};

pub(crate) struct OperatorStatistics<'a> {
    index: &'a IndexStats,
    candidates: &'a [IndexScanCandidate],
    rows: &'a [usize],
}

impl<'a> OperatorStatistics<'a> {
    pub(crate) fn new(
        index: &'a IndexStats,
        candidates: &'a [IndexScanCandidate],
        rows: &'a [usize],
    ) -> Self {
        debug_assert_eq!(candidates.len(), rows.len());
        Self {
            index,
            candidates,
            rows,
        }
    }

    pub(crate) fn scan(
        &self,
        name: &str,
        field: &str,
        predicate: &Predicate,
    ) -> Option<(f64, f64)> {
        self.candidates
            .iter()
            .zip(self.rows)
            .find_map(|(candidate, &rows)| {
                (candidate.index_name == name
                    && candidate.field == field
                    && &candidate.predicate == predicate)
                    .then_some((rows as f64, candidate.scan_cost))
            })
    }
}

impl<'a> From<&'a IndexStats> for OperatorStatistics<'a> {
    fn from(index: &'a IndexStats) -> Self {
        Self::new(index, &[], &[])
    }
}

impl std::ops::Deref for OperatorStatistics<'_> {
    type Target = IndexStats;
    fn deref(&self) -> &IndexStats {
        self.index
    }
}
