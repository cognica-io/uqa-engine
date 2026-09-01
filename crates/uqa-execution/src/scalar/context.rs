//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical scalar evaluation context and runtime capabilities.

use uqa_sql::expr::{EngineHook, EvalContext, RowLookup};
use uqa_sql::{ResultRow, SQLParam};

use crate::batch::{PhysicalRow, RowSchema};

use super::subquery::ScalarSubqueryRunner;

pub struct ScalarEvalContext<'a> {
    row: Option<&'a ResultRow>,
    row_lookup: Option<&'a dyn RowLookup>,
    row_schema: Option<&'a RowSchema>,
    params: &'a [SQLParam],
    function_hook: Option<&'a dyn EngineHook>,
    subquery_runner: Option<&'a dyn ScalarSubqueryRunner>,
    physical_outer_row: Option<(&'a RowSchema, &'a PhysicalRow)>,
}

impl<'a> ScalarEvalContext<'a> {
    #[must_use]
    pub fn new(row: Option<&'a ResultRow>, params: &'a [SQLParam]) -> Self {
        Self {
            row,
            row_lookup: row.map(|row| row as &dyn RowLookup),
            row_schema: None,
            params,
            function_hook: None,
            subquery_runner: None,
            physical_outer_row: None,
        }
    }

    #[must_use]
    pub fn from_row_lookup(row: &'a dyn RowLookup, params: &'a [SQLParam]) -> Self {
        Self {
            row: None,
            row_lookup: Some(row),
            row_schema: None,
            params,
            function_hook: None,
            subquery_runner: None,
            physical_outer_row: None,
        }
    }

    #[must_use]
    pub fn with_function_hook(mut self, hook: &'a dyn EngineHook) -> Self {
        self.function_hook = Some(hook);
        self
    }

    #[must_use]
    pub fn with_row_schema(mut self, schema: &'a RowSchema) -> Self {
        self.row_schema = Some(schema);
        self
    }

    #[must_use]
    pub fn with_subquery_runner(mut self, runner: &'a dyn ScalarSubqueryRunner) -> Self {
        self.subquery_runner = Some(runner);
        self
    }

    #[must_use]
    pub fn with_physical_outer_row(mut self, schema: &'a RowSchema, row: &'a PhysicalRow) -> Self {
        self.physical_outer_row = Some((schema, row));
        self
    }

    pub(super) fn sql_context(&self) -> EvalContext<'_> {
        let context = self.row_lookup.map_or_else(
            || EvalContext::new(self.row, self.params),
            |row| EvalContext::from_row_lookup(row, self.params),
        );
        match self.function_hook {
            Some(hook) => context.with_engine(hook),
            None => context,
        }
    }

    pub(super) fn outer_row(&self) -> Option<&dyn RowLookup> {
        self.row_lookup
    }

    pub(super) fn row_lookup(&self) -> Option<&'a dyn RowLookup> {
        self.row_lookup
    }

    pub(super) fn row_schema(&self) -> Option<&'a RowSchema> {
        self.row_schema
    }

    pub(super) fn params(&self) -> &'a [SQLParam] {
        self.params
    }

    pub(super) fn function_hook(&self) -> Option<&'a dyn EngineHook> {
        self.function_hook
    }

    pub(super) fn subquery_runner(&self) -> Option<&'a dyn ScalarSubqueryRunner> {
        self.subquery_runner
    }

    pub(super) fn physical_outer_row(&self) -> Option<(&'a RowSchema, &'a PhysicalRow)> {
        self.physical_outer_row
    }
}
