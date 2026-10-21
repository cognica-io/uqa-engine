//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered support merges for complete text inputs whose only payload is a score.

use super::{CancellationToken, CapturedScoredEntries, SQLError, ScoredEntry, ScoredEntryCapture};

impl CapturedScoredEntries {
    /// Both inputs have unique, ascending identities. Keep the original left-to-right score addition and preserve unmatched scores only for a union.
    pub(crate) fn merge_text_scores(
        self,
        right: Self,
        intersection: bool,
        bytes: usize,
        cancellation: CancellationToken,
    ) -> Result<Self, SQLError> {
        self.merge_ordered_scores(right, intersection, bytes, cancellation, |left, right| {
            left + right
        })
    }

    /// All-field text retrieval selects the maximum field score in the original field order, independently of Boolean score addition.
    pub(crate) fn merge_field_scores(
        self,
        right: Self,
        bytes: usize,
        cancellation: CancellationToken,
    ) -> Result<Self, SQLError> {
        self.merge_ordered_scores(right, false, bytes, cancellation, f64::max)
    }

    fn merge_ordered_scores(
        mut self,
        mut right: Self,
        intersection: bool,
        bytes: usize,
        cancellation: CancellationToken,
        merge: impl Fn(f64, f64) -> f64,
    ) -> Result<Self, SQLError> {
        let mut output = ScoredEntryCapture::new(bytes, cancellation)?;
        let mut left_entry = self.next().transpose()?;
        let mut right_entry = right.next().transpose()?;
        while let (Some(left), Some(other)) = (left_entry.as_ref(), right_entry.as_ref()) {
            match left.doc_id.cmp(&other.doc_id) {
                std::cmp::Ordering::Equal => {
                    output.push(ScoredEntry {
                        doc_id: left.doc_id,
                        score: merge(left.score, other.score),
                    })?;
                    left_entry = self.next().transpose()?;
                    right_entry = right.next().transpose()?;
                }
                std::cmp::Ordering::Less => {
                    if !intersection {
                        output.push(left.clone())?;
                    }
                    left_entry = self.next().transpose()?;
                }
                std::cmp::Ordering::Greater => {
                    if !intersection {
                        output.push(other.clone())?;
                    }
                    right_entry = right.next().transpose()?;
                }
            }
        }
        if !intersection {
            for entry in left_entry.into_iter().map(Ok).chain(self) {
                output.push(entry?)?;
            }
            for entry in right_entry.into_iter().map(Ok).chain(right) {
                output.push(entry?)?;
            }
        }
        output.finish()
    }
}
