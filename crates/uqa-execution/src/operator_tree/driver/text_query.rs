//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete Boolean text scores and their calibration units from the same execution.

use super::{
    scored_term_count, DriverResult, OperatorTree, PhysicalRetrievalDriver, PostingList, SQLError,
};

impl PhysicalRetrievalDriver<'_> {
    pub(super) fn execute_counted_text_query(
        &self,
        source: &OperatorTree,
    ) -> DriverResult<(PostingList, usize)> {
        match source {
            OperatorTree::Phrase {
                query,
                field,
                scoring,
            } => self.execute_phrase_counted(query, field.as_deref(), *scoring),
            OperatorTree::Intersect(children) | OperatorTree::Union(children) => {
                let mut result: Option<PostingList> = None;
                let mut units = 0_usize;
                for child in children {
                    self.context.runtime.check_cancelled()?;
                    let (rows, count) = self.execute_counted_text_query(child)?;
                    units = units.checked_add(count).ok_or_else(|| {
                        SQLError::Internal("text query calibration count overflow".into())
                    })?;
                    result = Some(match result {
                        None => rows,
                        Some(previous) if matches!(source, OperatorTree::Intersect(_)) => {
                            previous.merge_intersection_owned(&rows)
                        }
                        Some(previous) => previous.merge_union(&rows),
                    });
                }
                Ok((result.unwrap_or_default(), units))
            }
            // Complements have no scoring contribution. Other carriers keep their existing calibration contract.
            _ => self
                .execute_posting_node(source)
                .map(|rows| (rows, scored_term_count(source))),
        }
    }
}
