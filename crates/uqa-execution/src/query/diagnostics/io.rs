//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One measured operation owns an admitted observation under its current invocation.

use uqa_core::{
    io_usage::{IOContext, IOObservation, IOScope, IOUsage},
    memory::MemoryBudget,
};
use uqa_sql::SQLError;

pub(crate) struct Operation {
    observation: IOObservation,
    _scope: IOScope,
}

impl Operation {
    pub(crate) fn enter(
        parent: &IOContext,
        budget: &MemoryBudget,
    ) -> Result<Option<Self>, SQLError> {
        let Some(parent) = parent.observation() else {
            return Ok(None);
        };
        let observation = parent.child(budget)?;
        let scope = IOContext::new(Some(observation.clone())).enter();
        Ok(Some(Self {
            observation,
            _scope: scope,
        }))
    }

    pub(crate) fn finish(&self) -> Result<IOUsage, SQLError> {
        self.observation.finish().map_err(|_| {
            SQLError::Internal("EXPLAIN native I/O measurements could not be completed".into())
        })
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        let _ = self.observation.finish();
    }
}
