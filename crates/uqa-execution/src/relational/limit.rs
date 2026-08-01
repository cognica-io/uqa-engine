//! Streaming LIMIT/OFFSET.

use super::{Batch, ExecResult, PhysicalOperator, RowSchema};

pub struct Limit<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    offset: u64,
    limit: Option<u64>,
    skipped: u64,
    emitted: u64,
    schema: RowSchema,
}

impl<'a> Limit<'a> {
    pub fn new(child: Box<dyn PhysicalOperator + 'a>, offset: u64, limit: Option<u64>) -> Self {
        let schema = RowSchema::new(child.schema().to_vec());
        Self {
            child,
            offset,
            limit,
            skipped: 0,
            emitted: 0,
            schema,
        }
    }
}

impl PhysicalOperator for Limit<'_> {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.skipped = 0;
        self.emitted = 0;
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if matches!(self.limit, Some(0)) {
            return Ok(None);
        }
        loop {
            let Some(batch) = self.child.next()? else {
                return Ok(None);
            };
            let mut buf = Vec::new();
            for row in batch.rows {
                if self.skipped < self.offset {
                    self.skipped += 1;
                    continue;
                }
                if let Some(lim) = self.limit {
                    if self.emitted >= lim {
                        return if buf.is_empty() {
                            Ok(None)
                        } else {
                            Ok(Some(Batch::new(self.schema.clone(), buf)))
                        };
                    }
                }
                buf.push(row);
                self.emitted += 1;
            }
            if !buf.is_empty() {
                return Ok(Some(Batch::new(self.schema.clone(), buf)));
            }
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}
