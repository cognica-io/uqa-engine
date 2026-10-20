//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Completed mutation output remains bounded until a public caller requests owned SQL rows.

use super::returning::{ReturningBatchReader, ReturningRows};
use crate::{
    physical::{physical_exec_error, ExecError, ExecResult},
    Batch, OwnedPhysicalRow, PhysicalOperator, RowSchema,
};
use std::collections::BTreeSet;
use uqa_sql::{SQLError, SQLResult};

pub struct MutationOutput {
    pub schema: RowSchema,
    pub affected_rows: u64,
    rows: Option<ReturningBatchReader>,
    row_count: usize,
}

impl MutationOutput {
    pub fn from_affected(affected_rows: u64) -> Self {
        Self {
            schema: RowSchema::new(Vec::new()),
            affected_rows,
            rows: None,
            row_count: 0,
        }
    }

    pub(crate) fn from_returning(
        schema: RowSchema,
        rows: ReturningRows,
        affected_rows: u64,
    ) -> Result<Self, SQLError> {
        if !schema.is_identity_layout()
            || schema.physical_width() != schema.len()
            || rows
                .schema()
                .is_some_and(|source| source.columns() != schema.columns())
        {
            return Err(SQLError::Internal(
                "RETURNING values do not match their bound output columns".into(),
            ));
        }
        let row_count = rows.len();
        Ok(Self {
            schema,
            affected_rows,
            rows: Some(rows.into_batches()?),
            row_count,
        })
    }

    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn into_batches(self) -> MutationBatches {
        MutationBatches {
            schema: self.schema,
            rows: self.rows,
        }
    }

    pub fn into_operator(self) -> Box<dyn PhysicalOperator> {
        Box::new(MutationScan {
            count: self.row_count,
            batches: self.into_batches(),
            opened: false,
        })
    }

    pub fn into_sql_result(self) -> Result<SQLResult, SQLError> {
        let schema = self.schema.clone();
        let affected = self.affected_rows;
        let preserve_positions =
            schema.columns().iter().collect::<BTreeSet<_>>().len() != schema.len();
        let mut named_rows = Vec::with_capacity(self.row_count);
        let mut positional_rows = preserve_positions.then(|| Vec::with_capacity(self.row_count));
        for batch in self.into_batches() {
            let batch = batch?;
            for row in batch.rows {
                let row = OwnedPhysicalRow::new(batch.schema.clone(), row);
                if let Some(positional_rows) = positional_rows.as_mut() {
                    positional_rows
                        .push(row.view().iter().map(|(_, value)| value.clone()).collect());
                }
                named_rows.push(row.into_result_row());
            }
        }
        let mut result =
            SQLResult::from_schema_rows_with_positions(schema, named_rows, positional_rows);
        result.affected_rows = affected;
        Ok(result)
    }
}

pub struct MutationBatches {
    schema: RowSchema,
    rows: Option<ReturningBatchReader>,
}

impl Iterator for MutationBatches {
    type Item = Result<Batch, SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.rows.as_mut()?.next().map(|batch| {
            let mut batch = batch.map_err(physical_exec_error)?;
            // The producer retained dense positional values; final binding owns the public types, collations and record descriptors.
            batch.schema = self.schema.clone();
            Ok(batch)
        })
    }
}

struct MutationScan {
    count: usize,
    batches: MutationBatches,
    opened: bool,
}

impl PhysicalOperator for MutationScan {
    fn row_schema(&self) -> &RowSchema {
        &self.batches.schema
    }
    fn estimated_cardinality(&self) -> Option<u64> {
        u64::try_from(self.count).ok()
    }
    fn open(&mut self) -> ExecResult<()> {
        if self.opened {
            return Err(ExecError::Other(
                "mutation output cannot be reopened".into(),
            ));
        }
        self.opened = true;
        Ok(())
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.batches.next().transpose().map_err(ExecError::from)
    }
    fn close(&mut self) -> ExecResult<()> {
        self.batches.rows = None;
        Ok(())
    }
}
