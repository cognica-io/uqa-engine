//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result};
use crate::{
    ast::{Expr, SelectStmt},
    plan::{AggregateClassifier, QueryPlan},
};

pub(super) mod block;
pub(super) mod construct;

impl Copying<'_> {
    pub(super) fn lower_query_fields(
        &mut self,
        statement: &SelectStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<QueryPlan> {
        self.lower_query_source(construct::Source::Select(statement), aggregates)
    }

    pub(super) fn lower_values_query(
        &mut self,
        rows: &[Vec<Expr>],
        aggregates: &dyn AggregateClassifier,
    ) -> Result<QueryPlan> {
        self.lower_query_source(construct::Source::Values(rows), aggregates)
    }

    fn lower_query_source(
        &mut self,
        source: construct::Source<'_>,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<QueryPlan> {
        let mut scope = self.enter_plan_node()?;
        let copying = &mut *scope;
        let mut input = block::Inline {
            aggregates,
            arena: Some(copying.query_arena()),
        };
        let mut query = copying.retain_plan(crate::plan::retirement::empty_query());
        crate::catalog::work::inline(construct::construct(
            source, copying, &mut input, &mut query,
        ))?;
        Ok(query.take())
    }
}
