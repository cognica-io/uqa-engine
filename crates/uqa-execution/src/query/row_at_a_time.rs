//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

/// Preserve row-at-a-time demand across the scalar projection immediately below `LockRows`. `PostgreSQL` evaluates that projection for the candidate it is about to lock, but it does not evaluate a whole vectorized batch after an enclosing LIMIT has already obtained enough locked rows.
pub struct RowAtATime<'a> {
    input: Box<dyn crate::PhysicalOperator + 'a>,
    schema: crate::RowSchema,
    ordering: Vec<crate::PhysicalOrder>,
    pending: std::vec::IntoIter<crate::PhysicalRow>,
}

impl<'a> RowAtATime<'a> {
    pub fn new(input: Box<dyn crate::PhysicalOperator + 'a>) -> Self {
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

impl crate::PhysicalOperator for RowAtATime<'_> {
    fn row_schema(&self) -> &crate::RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.input.estimated_cardinality()
    }

    fn output_ordering(&self) -> &[crate::PhysicalOrder] {
        &self.ordering
    }

    fn backward_scan_support(&self) -> crate::BackwardScanSupport {
        self.input.backward_scan_support()
    }

    fn open(&mut self) -> crate::ExecResult<()> {
        self.pending = Vec::new().into_iter();
        self.input.open()
    }

    fn next(&mut self) -> crate::ExecResult<Option<crate::Batch>> {
        loop {
            if let Some(row) = self.pending.next() {
                return Ok(Some(crate::Batch::from_physical_rows(
                    self.schema.clone(),
                    vec![row],
                )));
            }
            let Some(batch) = self.input.next()? else {
                return Ok(None);
            };
            if batch.schema != self.schema {
                return Err(crate::ExecError::Other(format!(
                    "row-at-a-time input schema mismatch: expected {:?}, got {:?}",
                    self.schema, batch.schema
                )));
            }
            self.pending = batch.rows.into_iter();
        }
    }

    fn next_direction(
        &mut self,
        direction: crate::PhysicalScanDirection,
    ) -> crate::ExecResult<Option<crate::Batch>> {
        if self.pending.len() != 0 {
            return Err(crate::ExecError::Other(
                "row-at-a-time operator cannot mix batched and directional pulls".into(),
            ));
        }
        let Some(batch) = self.input.next_direction(direction)? else {
            return Ok(None);
        };
        if batch.schema != self.schema {
            return Err(crate::ExecError::Other(format!(
                "row-at-a-time input schema mismatch: expected {:?}, got {:?}",
                self.schema, batch.schema
            )));
        }
        if batch.rows.len() != 1 {
            return Err(crate::ExecError::Other(format!(
                "directional row-at-a-time input returned {} rows",
                batch.rows.len()
            )));
        }
        Ok(Some(batch))
    }

    fn rewind(&mut self) -> crate::ExecResult<()> {
        self.pending = Vec::new().into_iter();
        self.input.rewind()
    }

    fn close(&mut self) -> crate::ExecResult<()> {
        self.pending = Vec::new().into_iter();
        self.input.close()
    }
}
