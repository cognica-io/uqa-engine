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
        retirement::construction::{Context, Prefix},
        scalar::{lower_scalar_with_arena, QueryLowering},
        AggregateClassifier, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
        QueryPlan, UnifiedPlan,
    },
    schema::retention::CatalogRetentionError,
};
use uqa_core::memory::{BudgetedVec, ProductionControl};

macro_rules! field {
    ($copying:ident, $name:literal, |$field:ident| $body:expr) => {{
        let mut $field = $copying.begin_input_field()?;
        let value = $body;
        $copying.finish_input_field($name, $field);
        value
    }};
}

mod commands;
mod ctes;
mod expressions;
pub(super) mod input;
mod mutations;
mod ownership;
mod projections;
mod query;
mod rows;
mod source;
mod windows;

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
        let mut arena = QueryArena::new(*control, None, Context::new(control)?.child(control)?);
        let scalar = lower_scalar_with_arena(expression, aggregates, &mut arena, *control)?;
        let (queries, memory) = arena.queries.take().into_parts();
        arena.copying.absorb(Some(memory));
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
                query, aggregates, control, parameters, None,
            )?)),
            Statement::Values { rows } => {
                copying.plan_retirement = Some(Context::new(control)?);
                let query = copying.lower_values_query(rows, aggregates)?;
                Ok(MutableExecutablePlan::Query(
                    MutableQueryPlan::from_rewrite_parts(
                        query,
                        Some(copying.into_rewrite_memory()),
                    ),
                ))
            }
            statement => {
                copying.plan_retirement = Some(Context::new(control)?);
                let command = copying.lower_command(statement, aggregates)?;
                let command = copying.retain_plan(command);
                control.check()?;
                Ok(MutableExecutablePlan::Command(
                    MutableCommandPlan::from_rewrite_parts(
                        command.take(),
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
    retirement: Option<Context>,
) -> Result<MutableQueryPlan> {
    let mut copying = Copying::for_rewrite(*control).expect("admitted syntax lowering");
    copying.parameters = parameters;
    copying.plan_retirement = Some(match retirement {
        Some(retirement) => retirement,
        None => Context::new(control)?,
    });
    let query = copying.lower_query_fields(statement, aggregates)?;
    let query = copying.retain_plan(query);
    let memory = copying.into_rewrite_memory();
    query
        .map(|query| MutableQueryPlan::from_rewrite_parts(query, Some(memory)))
        .normalize_window_definitions(control)
        .map(Prefix::take)
}

struct QueryArena<'a> {
    queries: Prefix<BudgetedVec<QueryPlan>>,
    copying: Copying<'a>,
}

impl<'a> QueryArena<'a> {
    fn new(
        control: ProductionControl<'a>,
        parameters: Option<&'a ParameterLookup<'a>>,
        retirement: Context,
    ) -> Self {
        let mut copying = Copying::for_rewrite(control).expect("admitted query arena");
        copying.parameters = parameters;
        copying.plan_retirement = Some(retirement.clone());
        Self {
            copying,
            queries: retirement.retain(BudgetedVec::new(
                control.budget().expect("admitted query arena"),
            )),
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
            let query = lower_query(
                statement,
                aggregates,
                &copying.control,
                copying.parameters,
                copying.plan_retirement.clone(),
            )?;
            copying.take_query(query)
        })?;
        self.queries.push(query)?;
        Ok(index)
    }
}

impl<'c> Copying<'c> {
    fn query_arena(&self) -> QueryArena<'c> {
        QueryArena::new(
            self.control,
            self.parameters,
            self.plan_retirement
                .as_ref()
                .expect("active query constructor")
                .clone(),
        )
    }

    fn take_arena(&mut self, mut arena: QueryArena<'_>) -> Result<Vec<QueryPlan>> {
        self.check()?;
        self.fields.as_mut().expect("query fields").reserve(1)?;
        let (queries, memory) = arena.queries.take().into_parts();
        arena.copying.absorb(Some(memory));
        self.fields
            .as_mut()
            .expect("query fields")
            .push(RewriteField {
                name: "subqueries",
                memory: Some(arena.copying.into_rewrite_memory()),
            })
            .expect("query arena field was admitted");
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
            self.plan_retirement.clone(),
        )?)
    }

    fn lower_statement(
        &mut self,
        statement: &Statement,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<UnifiedPlan> {
        Ok(match statement {
            Statement::Select(query) => UnifiedPlan::Query(
                self.lower_boxed(|copying| copying.lower_nested_query(query, aggregates))?,
            ),
            Statement::Values { rows } => UnifiedPlan::Query(self.lower_boxed(|copying| {
                copying.field("query", |copying| {
                    copying.lower_values_query(rows, aggregates)
                })
            })?),
            statement => UnifiedPlan::Command(self.lower_boxed(|copying| {
                copying.field("command", |copying| {
                    copying.lower_command(statement, aggregates)
                })
            })?),
        })
    }
}
