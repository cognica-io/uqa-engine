//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed syntax produces its first executable destination under the selected allowance.

use super::{syntax::SyntaxCopy, Copying, ParameterLookup, Result, RewriteField};
use crate::{
    ast::{Expr, SelectStmt, Statement},
    plan::{
        scalar::{lower_scalar_with_arena, QueryLowering},
        AggregateClassifier, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
        QueryPlan, UnifiedPlan,
    },
    schema::retention::CatalogRetentionError,
};
use uqa_core::memory::{BudgetedVec, ProductionControl};

mod commands;
mod ctes;
mod expressions;
mod mutations;
mod query;
mod source;

impl crate::plan::ExpressionPlan {
    /// Lower a borrowed expression and every nested query directly into independently movable owners.
    pub fn lower_borrowed_for_rewrite(
        expression: &Expr,
        control: &ProductionControl<'_>,
    ) -> Result<crate::plan::MutableExpressionPlan> {
        Self::lower_borrowed_with(expression, &crate::plan::NoRegisteredAggregates, control)
    }

    pub fn lower_borrowed_with(
        expression: &Expr,
        aggregates: &dyn AggregateClassifier,
        control: &ProductionControl<'_>,
    ) -> Result<crate::plan::MutableExpressionPlan> {
        control.check()?;
        if control.budget().is_none() {
            return Self::lower_with(expression.clone(), aggregates).copy_for_rewrite(control);
        }
        let mut arena = QueryArena::new(*control, None);
        let scalar = lower_scalar_with_arena(expression, aggregates, &mut arena, *control)?;
        let (queries, memory) = arena.queries.into_parts();
        arena.copying.memory.absorb(memory);
        let arena = crate::plan::ScalarQueryArena::from_rewrite_parts(
            queries,
            Some(arena.copying.into_rewrite_memory()),
        );
        crate::plan::MutableExpressionPlan::from_parts(scalar, arena, control)
    }
}

impl UnifiedPlan {
    /// Lower retained syntax directly into independently owned executable fields. Source parsing and its original syntax owner remain separate; no temporary ordinary executable is constructed on the controlled path.
    pub fn lower_borrowed_for_rewrite(
        statement: &Statement,
        aggregates: &dyn AggregateClassifier,
        control: &ProductionControl<'_>,
    ) -> Result<MutableExecutablePlan> {
        Self::lower_borrowed_with_parameters(statement, aggregates, control, None)
    }

    pub(crate) fn lower_borrowed_with_parameters(
        statement: &Statement,
        aggregates: &dyn AggregateClassifier,
        control: &ProductionControl<'_>,
        parameters: Option<&ParameterLookup<'_>>,
    ) -> Result<MutableExecutablePlan> {
        control.check()?;
        let Some(mut copying) = Copying::for_rewrite(*control) else {
            if parameters.is_some() {
                return Err(crate::SQLError::Internal(
                    "parameter substitution requires an owning syntax producer".into(),
                ));
            }
            return Ok(match Self::lower_with(statement.clone(), aggregates) {
                Self::Query(query) => {
                    MutableExecutablePlan::Query(MutableQueryPlan::from_rewrite_parts(*query, None))
                }
                Self::Command(command) => MutableExecutablePlan::Command(
                    MutableCommandPlan::from_rewrite_parts(*command, None),
                ),
            });
        };
        copying.parameters = parameters;
        match statement {
            Statement::Select(query) => Ok(MutableExecutablePlan::Query(lower_query(
                query, aggregates, control, parameters,
            )?)),
            Statement::Values { rows } => {
                let query = copying.lower_values_query(rows, aggregates)?;
                Ok(MutableExecutablePlan::Query(
                    MutableQueryPlan::from_rewrite_parts(
                        query,
                        Some(copying.into_rewrite_memory()),
                    ),
                ))
            }
            statement => {
                let command = copying.lower_command(statement, aggregates)?;
                control.check()?;
                Ok(MutableExecutablePlan::Command(
                    MutableCommandPlan::from_rewrite_parts(
                        command,
                        Some(copying.into_rewrite_memory()),
                    ),
                ))
            }
        }
    }
}

fn lower_query(
    statement: &SelectStmt,
    aggregates: &dyn AggregateClassifier,
    control: &ProductionControl<'_>,
    parameters: Option<&ParameterLookup<'_>>,
) -> Result<MutableQueryPlan> {
    let mut copying = Copying::for_rewrite(*control).expect("admitted syntax lowering");
    copying.parameters = parameters;
    let query = copying.lower_query_fields(statement, aggregates)?;
    MutableQueryPlan::from_rewrite_parts(query, Some(copying.into_rewrite_memory()))
        .normalize_window_definitions(control)
}

struct QueryArena<'a> {
    copying: Copying<'a>,
    queries: BudgetedVec<QueryPlan>,
}

impl<'a> QueryArena<'a> {
    fn new(control: ProductionControl<'a>, parameters: Option<&'a ParameterLookup<'a>>) -> Self {
        let mut copying = Copying::for_rewrite(control).expect("admitted query arena");
        copying.parameters = parameters;
        Self {
            copying,
            queries: BudgetedVec::new(control.budget().expect("admitted query arena")),
        }
    }
}

impl QueryLowering for QueryArena<'_> {
    fn parameter(&self, expression: &Expr) -> Option<usize> {
        self.copying
            .parameters
            .and_then(|parameters| parameters(expression))
    }
    fn append_owned(
        &mut self,
        statement: SelectStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> std::result::Result<usize, CatalogRetentionError> {
        self.append_borrowed(&statement, aggregates)
    }

    fn append_borrowed(
        &mut self,
        statement: &SelectStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> std::result::Result<usize, CatalogRetentionError> {
        self.copying.check()?;
        self.queries.reserve(1)?;
        let index = self.queries.len();
        let query = self.copying.field("item", |copying| {
            let query = lower_query(statement, aggregates, &copying.control, copying.parameters)?;
            copying.take_query(query)
        })?;
        self.queries.push(query)?;
        Ok(index)
    }
}

impl Copying<'_> {
    fn take_arena(&mut self, mut arena: QueryArena<'_>) -> Result<Vec<QueryPlan>> {
        self.check()?;
        self.fields.as_mut().expect("query fields").reserve(1)?;
        let (queries, memory) = arena.queries.into_parts();
        arena.copying.memory.absorb(memory);
        self.fields
            .as_mut()
            .expect("query fields")
            .push(RewriteField {
                name: "subqueries",
                memory: Some(arena.copying.into_rewrite_memory()),
            })?;
        Ok(queries)
    }

    fn lower_scalar(
        &mut self,
        expression: &Expr,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<crate::ScalarExpr> {
        let expression = lower_scalar_with_arena(expression, aggregates, arena, self.control)?;
        self.take_scalar(expression)
    }

    fn lower_nested_query(
        &mut self,
        statement: &SelectStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<QueryPlan> {
        self.take_query(lower_query(
            statement,
            aggregates,
            &self.control,
            self.parameters,
        )?)
    }

    fn lower_statement(
        &mut self,
        statement: &Statement,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<UnifiedPlan> {
        Ok(match statement {
            Statement::Select(query) => UnifiedPlan::Query(
                self.boxed(|copying| copying.lower_nested_query(query, aggregates))?,
            ),
            Statement::Values { rows } => UnifiedPlan::Query(self.boxed(|copying| {
                copying.field("query", |copying| {
                    copying.lower_values_query(rows, aggregates)
                })
            })?),
            statement => UnifiedPlan::Command(self.boxed(|copying| {
                copying.field("command", |copying| {
                    copying.lower_command(statement, aggregates)
                })
            })?),
        })
    }
}
