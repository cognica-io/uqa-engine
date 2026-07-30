//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sparse threshold operator (Section 6.5, Paper 4).
//!
//! `SparseThresholdOperator` shifts every score by `-threshold` and
//! drops entries whose adjusted score is non-positive. This realizes
//! the MAP-estimation interpretation of `ReLU` activation: documents
//! below the threshold have zero posterior under the sparse prior.

use std::sync::Arc;

use uqa_core::{IndexStats, Payload, PostingEntry, PostingList};
use uqa_storage::StorageBackendError;

use crate::base::{require_finite_score, ExecutionContext, Operator, OperatorResult};

pub struct SparseThresholdOperator {
    pub source: Arc<dyn Operator>,
    pub threshold: f64,
}

impl SparseThresholdOperator {
    pub fn new(source: Arc<dyn Operator>, threshold: f64) -> Self {
        Self { source, threshold }
    }
}

impl Operator for SparseThresholdOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        if !self.threshold.is_finite() {
            return Err(StorageBackendError::Other(
                "sparse threshold must be finite".to_string(),
            ));
        }
        let pl = self.source.execute(ctx)?;
        for entry in pl.entries() {
            require_finite_score(entry.payload.score, "sparse threshold")?;
        }
        let entries: Vec<PostingEntry> = pl
            .entries()
            .iter()
            .filter_map(|e| {
                let adjusted = e.payload.score - self.threshold;
                if adjusted > 0.0 {
                    Some(PostingEntry::new(
                        e.doc_id,
                        Payload {
                            positions: e.payload.positions.clone(),
                            score: adjusted,
                            fields: e.payload.fields.clone(),
                        },
                    ))
                } else {
                    None
                }
            })
            .collect();
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.source.cost_estimate(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ConstOperator(Vec<PostingEntry>);

    impl Operator for ConstOperator {
        fn execute(&self, _ctx: &ExecutionContext) -> OperatorResult {
            Ok(PostingList::from_sorted_unchecked(self.0.clone()))
        }
    }

    #[test]
    fn drops_entries_below_threshold_and_subtracts() {
        let source = Arc::new(ConstOperator(vec![
            PostingEntry::new(1, Payload::with_score(0.3)),
            PostingEntry::new(2, Payload::with_score(0.7)),
            PostingEntry::new(3, Payload::with_score(0.5)),
        ])) as Arc<dyn Operator>;
        let op = SparseThresholdOperator::new(source, 0.4);
        let out = op.execute(&ExecutionContext::new()).unwrap();
        let entries: Vec<(u64, f64)> = out
            .entries()
            .iter()
            .map(|e| (e.doc_id, e.payload.score))
            .collect();
        assert_eq!(entries.len(), 2);
        // Doc 2: 0.7 - 0.4 = 0.3.
        let pair_2 = entries.iter().find(|(id, _)| *id == 2).unwrap();
        assert!((pair_2.1 - 0.3).abs() < 1e-9);
        // Doc 1 is below threshold so dropped.
        assert!(entries.iter().all(|(id, _)| *id != 1));
    }
}
