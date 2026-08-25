//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One logical scored scan over independently stored hierarchy members.

use std::collections::VecDeque;

use uqa_execution::{ExecResult, PhysicalRow, RowSchema, RowSource};
use uqa_sql::ResultRow;

use super::{SQLError, ScoredDocumentSource};

/// Concatenate scored physical relation sources without collapsing their
/// table-local document identifiers. Each child source materializes against
/// its own table and carries its own row-lock origin, while this adapter
/// exposes the selected ancestor's one logical row type.
pub(in crate::sql) struct HierarchyScoredDocumentSource {
    sources: VecDeque<ScoredDocumentSource>,
    schema: Vec<String>,
    physical_schema: RowSchema,
    estimated_cardinality: u64,
}

impl HierarchyScoredDocumentSource {
    pub(in crate::sql) fn new(
        sources: Vec<ScoredDocumentSource>,
        estimated_cardinality: usize,
    ) -> Result<Self, SQLError> {
        let first = sources.first().ok_or_else(|| {
            SQLError::Internal("hierarchy retrieval was built without a physical table".into())
        })?;
        let schema = first.schema().to_vec();
        let physical_schema = first
            .physical_schema()
            .cloned()
            .ok_or_else(|| SQLError::Internal("scored source has no physical schema".into()))?;
        for source in sources.iter().skip(1) {
            if source.schema() != schema
                || source.physical_schema().is_none_or(|candidate| {
                    candidate.identities() != physical_schema.identities()
                        || candidate.column_types() != physical_schema.column_types()
                })
            {
                return Err(SQLError::Internal(
                    "hierarchy retrieval sources do not share one logical row type".into(),
                ));
            }
        }
        let estimated_cardinality = u64::try_from(estimated_cardinality).map_err(|_| {
            SQLError::Internal("hierarchy retrieval cardinality exceeds u64".into())
        })?;
        Ok(Self {
            sources: sources.into(),
            schema,
            physical_schema,
            estimated_cardinality,
        })
    }
}

impl RowSource for HierarchyScoredDocumentSource {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn physical_schema(&self) -> Option<&RowSchema> {
        Some(&self.physical_schema)
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        Some(self.estimated_cardinality)
    }

    fn next_row(&mut self) -> ExecResult<Option<ResultRow>> {
        Ok(self.next_batch(1)?.pop())
    }

    fn next_batch(&mut self, max_rows: usize) -> ExecResult<Vec<ResultRow>> {
        let rows = self.next_physical_batch(max_rows)?;
        Ok(rows
            .iter()
            .map(|row| self.physical_schema.view(row).to_result_row())
            .collect())
    }

    fn next_physical_batch(&mut self, max_rows: usize) -> ExecResult<Vec<PhysicalRow>> {
        let mut rows = Vec::with_capacity(max_rows);
        while rows.len() < max_rows {
            let Some(source) = self.sources.front_mut() else {
                break;
            };
            let mut batch = source.next_physical_batch(max_rows - rows.len())?;
            if batch.is_empty() {
                self.sources.pop_front();
            } else {
                rows.append(&mut batch);
            }
        }
        Ok(rows)
    }
}
