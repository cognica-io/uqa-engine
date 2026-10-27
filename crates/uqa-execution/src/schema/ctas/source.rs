//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain one selected CTAS source through row locking, writer promotion and output collection.

use super::{CreateTableAsExecution, TableAsQuerySource};
use crate::query::measurement::QueryMeasurement;
use std::borrow::Cow;
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam, SQLResult};

pub(super) struct SelectedSource<'a> {
    queries: &'a dyn TableAsQuerySource,
    query: Cow<'a, QueryPlan>,
    params: &'a [SQLParam],
    measurement: Option<&'a QueryMeasurement>,
}

impl<'a> SelectedSource<'a> {
    pub(super) fn new(
        queries: &'a dyn TableAsQuerySource,
        execution: &CreateTableAsExecution<'a>,
    ) -> Result<Self, SQLError> {
        let (query, measurement) = if let Some(query) = execution.explained {
            (Cow::Borrowed(query.plan), Some(query.measurement))
        } else if execution.with_no_data {
            (Cow::Borrowed(execution.query), None)
        } else {
            (Cow::Owned(queries.optimize(execution.query)?), None)
        };
        Ok(Self {
            queries,
            query,
            params: execution.params,
            measurement,
        })
    }

    pub(super) fn has_row_locks(&self) -> bool {
        crate::query::locking::query_has_row_locks(&self.query)
    }

    pub(super) fn execute(&self) -> Result<SQLResult, SQLError> {
        self.queries
            .execute(&self.query, self.params, self.measurement)
    }
}
