//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming whole-row mapping adapter.

use std::sync::Arc;

use uqa_sql::ResultRow;

use crate::{Batch, ExecResult, PhysicalOperator, RowSchema};

pub type SharedRowMapper<'a> = Arc<dyn Fn(ResultRow) -> ExecResult<ResultRow> + Send + Sync + 'a>;

/// Apply a fallible mapping to each input row without changing batch
/// backpressure or collecting the child relation.
pub struct MapRows<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    mapper: SharedRowMapper<'a>,
    schema: RowSchema,
}

impl<'a> MapRows<'a> {
    pub fn new(
        child: Box<dyn PhysicalOperator + 'a>,
        schema: Vec<String>,
        mapper: SharedRowMapper<'a>,
    ) -> Self {
        Self {
            child,
            mapper,
            schema: RowSchema::new(schema),
        }
    }
}

impl PhysicalOperator for MapRows<'_> {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(batch) = self.child.next()? else {
            return Ok(None);
        };
        let rows = batch
            .rows
            .into_iter()
            .map(|row| (self.mapper)(row))
            .collect::<ExecResult<Vec<_>>>()?;
        Ok(Some(Batch::new(self.schema.clone(), rows)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use uqa_core::Value;

    use super::*;
    use crate::{physical::run_to_rows, ExecError, TableScan};

    #[test]
    fn mapper_preserves_late_errors() {
        let rows = vec![
            BTreeMap::from([("v".into(), Value::Int(1))]),
            BTreeMap::from([("v".into(), Value::Int(2))]),
        ];
        let child = Box::new(TableScan::from_rows(vec!["v".into()], rows));
        let mapper = Arc::new(|row: ResultRow| {
            if row.get("v") == Some(&Value::Int(2)) {
                Err(ExecError::Other("map failed".into()))
            } else {
                Ok(row)
            }
        });
        let mut operator = MapRows::new(child, vec!["v".into()], mapper);
        let error = run_to_rows(&mut operator).unwrap_err();
        assert!(error.to_string().contains("map failed"));
    }
}
