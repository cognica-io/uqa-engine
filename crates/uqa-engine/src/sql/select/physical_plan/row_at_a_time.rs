//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

/// Preserve row-at-a-time demand across the scalar projection immediately
/// below `LockRows`. `PostgreSQL` evaluates that projection for the candidate it
/// is about to lock, but it does not evaluate a whole vectorized batch after an
/// enclosing LIMIT has already obtained enough locked rows.
pub(super) struct RowAtATime<'a> {
    input: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    schema: uqa_execution::RowSchema,
    ordering: Vec<uqa_execution::PhysicalOrder>,
    pending: std::vec::IntoIter<uqa_execution::PhysicalRow>,
}

impl<'a> RowAtATime<'a> {
    pub(super) fn new(input: Box<dyn uqa_execution::PhysicalOperator + 'a>) -> Self {
        let schema = input.row_schema().clone();
        let ordering = input.output_ordering().to_vec();
        Self {
            input,
            schema,
            ordering,
            pending: Vec::new().into_iter(),
        }
    }
}

impl uqa_execution::PhysicalOperator for RowAtATime<'_> {
    fn row_schema(&self) -> &uqa_execution::RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.input.estimated_cardinality()
    }

    fn output_ordering(&self) -> &[uqa_execution::PhysicalOrder] {
        &self.ordering
    }

    fn open(&mut self) -> uqa_execution::ExecResult<()> {
        self.pending = Vec::new().into_iter();
        self.input.open()
    }

    fn next(&mut self) -> uqa_execution::ExecResult<Option<uqa_execution::Batch>> {
        loop {
            if let Some(row) = self.pending.next() {
                return Ok(Some(uqa_execution::Batch::from_physical_rows(
                    self.schema.clone(),
                    vec![row],
                )));
            }
            let Some(batch) = self.input.next()? else {
                return Ok(None);
            };
            self.pending = batch.rows.into_iter();
        }
    }

    fn close(&mut self) -> uqa_execution::ExecResult<()> {
        self.pending = Vec::new().into_iter();
        self.input.close()
    }
}
