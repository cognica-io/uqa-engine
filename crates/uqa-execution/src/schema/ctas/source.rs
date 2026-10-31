//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain one selected CTAS source through row locking, writer promotion and output collection.

use super::{CreateTableAsExecution, TableAsQuerySource};
use crate::query::measurement::QueryMeasurement;
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam, SQLResult};

pub(super) struct SelectedSource<'a> {
    queries: &'a dyn TableAsQuerySource,
    query: &'a QueryPlan,
    optimized: Option<uqa_sql::plan::OwnedQueryPlan>,
    params: &'a [SQLParam],
    measurement: Option<&'a QueryMeasurement>,
}

impl<'a> SelectedSource<'a> {
    pub(super) fn new(
        queries: &'a dyn TableAsQuerySource,
        execution: &CreateTableAsExecution<'a>,
    ) -> Result<Self, SQLError> {
        let optimized = if execution.explained.is_none() && !execution.with_no_data {
            Some(queries.optimize(execution.query)?)
        } else {
            None
        };
        let (query, measurement) = execution
            .explained
            .map_or((execution.query, None), |query| {
                (query.plan, Some(query.measurement))
            });
        Ok(Self {
            queries,
            query,
            optimized,
            params: execution.params,
            measurement,
        })
    }

    pub(super) fn has_row_locks(&self) -> bool {
        crate::query::locking::query_has_row_locks(self.optimized.as_deref().unwrap_or(self.query))
    }

    pub(super) fn execute(&self) -> Result<SQLResult, SQLError> {
        self.queries.execute(
            self.optimized.as_deref().unwrap_or(self.query),
            self.params,
            self.measurement,
        )
    }
}
