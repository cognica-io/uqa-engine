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

pub(crate) type RetrievalPredicate<'a> =
    dyn Fn(&str, &[crate::ScalarExpr]) -> Result<bool, uqa_sql::SQLError> + 'a;

#[derive(Clone, Copy)]
pub struct ScalarEvalContext<'a> {
    row: Option<&'a ResultRow>,
    row_lookup: Option<&'a dyn RowLookup>,
    row_schema: Option<&'a RowSchema>,
    params: &'a [SQLParam],
    function_hook: Option<&'a dyn EngineHook>,
    subquery_runner: Option<&'a dyn ScalarSubqueryRunner>,
    physical_outer_row: Option<(&'a RowSchema, &'a PhysicalRow)>,
    retrieval_predicate: Option<&'a RetrievalPredicate<'a>>,
    function_states: Option<&'a super::FunctionCallStates>,
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
            retrieval_predicate: None,
            function_states: None,
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
            retrieval_predicate: None,
            function_states: None,
        }
    }

    #[must_use]
    pub fn with_function_hook(mut self, hook: &'a dyn EngineHook) -> Self {
        self.function_hook = Some(hook);
        self
    }

    /// Retain the prepared expression's function state across its input rows.
    pub fn with_function_states(mut self, states: &'a super::FunctionCallStates) -> Self {
        self.function_states = Some(states);
        self
    }

    pub(super) fn enum_comparison_state(
        &self,
        arguments: &[super::ScalarExpr],
    ) -> Option<&uqa_sql::expr::enums::EnumComparisonState> {
        self.function_states
            .and_then(|states| states.enum_comparison(arguments))
    }

    pub(super) fn enum_binary_comparison_state(
        &self,
        left: &super::ScalarExpr,
    ) -> Option<&uqa_sql::expr::enums::EnumComparisonState> {
        self.function_states
            .and_then(|states| states.enum_binary_comparison(left))
    }

    pub(crate) fn with_retrieval_predicate(
        mut self,
        predicate: &'a RetrievalPredicate<'a>,
    ) -> Self {
        self.retrieval_predicate = Some(predicate);
        self
    }

    pub(crate) fn retrieval_predicate(&self) -> Option<&'a RetrievalPredicate<'a>> {
        self.retrieval_predicate
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

    pub(super) fn with_type_schema<R>(&self, operation: impl FnOnce(&RowSchema) -> R) -> R {
        match self.row_schema {
            Some(schema) => operation(schema),
            None => operation(&RowSchema::default()),
        }
    }

    pub(super) fn params(&self) -> &'a [SQLParam] {
        self.params
    }

    pub(crate) fn function_hook(&self) -> Option<&'a dyn EngineHook> {
        self.function_hook
    }

    pub(super) fn subquery_runner(&self) -> Option<&'a dyn ScalarSubqueryRunner> {
        self.subquery_runner
    }

    pub(super) fn physical_outer_row(&self) -> Option<(&'a RowSchema, &'a PhysicalRow)> {
        self.physical_outer_row
    }
}
