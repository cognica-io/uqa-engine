//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native statement scheduling uses the existing admitted field constructors and query arenas.

use super::{Copying, ScalarInput, SourceMetadataInput, TextInput};
use crate::{
    ast::{Expr, SelectStmt},
    construction::{self, Work},
    plan::{
        copy::lowering::{
            query::{block, construct},
            QueryArena, Result,
        },
        retirement::construction::Prefix,
        AggregateClassifier, ComputePlan, CtePlan, ExpressionPlan, MutableQueryPlan,
        OwnedScalarExpr, QueryBlockPlan, QueryPlan, SourcePlan,
    },
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

mod ctes;
mod mutations;
mod statement;
mod trees;
pub use statement::{RetainedExecutablePlan, StoredStatementObserver};

struct Native<'a, Q> {
    aggregates: &'a (dyn AggregateClassifier + Sync),
    work: Work<'a>,
    queries: Option<Q>,
}

pub(super) async fn scalar<'s: 'a, 'a, Q>(
    expression: &'s Expr,
    control: ProductionControl<'_>,
    work: &mut Work<'a>,
    queries: &mut Option<Q>,
) -> Result<OwnedScalarExpr>
where
    Q: crate::plan::scalar::ScalarQueryInput<&'s Expr, &'s SelectStmt> + Send + 'a,
{
    let mut input = queries.take().expect("active scalar query owner");
    let owner = control.capture();
    let output = work.submit(move |mut work| async move {
        let value = ExpressionPlan::lower_scalar_with_query_input(
            expression,
            &mut input,
            &owner.control(),
            &mut work,
        )?
        .await?;
        Ok::<_, SQLError>((value, input))
    })?;
    let (value, input) = construction::receive(output).await?;
    *queries = Some(input);
    Ok(value)
}

impl<'s: 'a, 'a, Q> ScalarInput<&'s Expr> for Native<'a, Q>
where
    Q: crate::plan::scalar::ScalarQueryInput<&'s Expr, &'s SelectStmt> + Send + 'a,
{
    async fn checkpoint(&mut self) -> Result<()> {
        self.work.checkpoint().await;
        Ok(())
    }
    async fn scalar(
        &mut self,
        expression: &'s Expr,
        control: ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr> {
        scalar(expression, control, &mut self.work, &mut self.queries).await
    }
}

impl<'s, Q: Send> TextInput<&'s str> for Native<'_, Q> {
    async fn text(
        &mut self,
        source: &'s str,
        control: ProductionControl<'_>,
    ) -> Result<Produced<String>> {
        crate::catalog::work::copy_text(&mut self.work, source, &control).await
    }
}

impl<'t, 'b, Q: Send>
    SourceMetadataInput<&'t crate::ast::ColumnType, &'b crate::ast::FunctionBinding>
    for Native<'_, Q>
{
    async fn ty(
        &mut self,
        source: &'t crate::ast::ColumnType,
        control: ProductionControl<'_>,
    ) -> Result<Produced<crate::ast::ColumnType>> {
        source.clone_with_input_work(&control, &mut self.work).await
    }
    async fn binding(
        &mut self,
        source: &'b crate::ast::FunctionBinding,
        control: ProductionControl<'_>,
    ) -> Result<Produced<crate::ast::FunctionBinding>> {
        source.clone_with_input_work(&control, &mut self.work).await
    }
}

pub(super) struct Arena<'a> {
    aggregates: &'a (dyn AggregateClassifier + Sync),
    value: QueryArena<'a>,
    work: Work<'a>,
}

impl<'a> Arena<'a> {
    fn new<'c: 'a>(
        copying: &Copying<'c>,
        work: Work<'a>,
        aggregates: &'a (dyn AggregateClassifier + Sync),
    ) -> Self {
        Self {
            aggregates,
            value: copying.query_arena(),
            work,
        }
    }
}

impl ExpressionPlan {
    /// Lower a standalone expression and its private query namespace using the same field owners as a statement. Each nested query contributes its slot before any later scalar occurrence is visited.
    pub(crate) async fn lower_borrowed_with_work<W: crate::catalog::work::Work + Send>(
        source: &Expr,
        aggregates: &(dyn AggregateClassifier + Sync),
        control: &ProductionControl<'_>,
        input: &mut W,
    ) -> Result<(
        crate::plan::MutableExpressionPlan,
        Option<crate::plan::copy::stored_plan::OriginalWorkspace>,
    )> {
        control.check()?;
        if !W::MAY_SUSPEND || control.budget().is_none() {
            return Ok((
                Self::lower_borrowed_with(source, aggregates, control)?,
                None,
            ));
        }
        input.checkpoint().await?;
        let mut driver = construction::Driver::new(*control)?;
        let output = driver.work().submit(move |mut work| async move {
            let retirement = crate::plan::retirement::construction::Context::new(control)?;
            let mut arena = Arena {
                aggregates,
                value: QueryArena::new(*control, None, retirement.child(control)?),
                work: work.clone(),
            };
            let scalar =
                Self::lower_scalar_with_query_input(source, &mut arena, control, &mut work)?
                    .await?;
            Ok((
                arena.value.finish_expression(scalar, control)?,
                Some(crate::plan::copy::stored_plan::OriginalWorkspace(
                    retirement,
                )),
            ))
        })?;
        while driver.advance().await? {
            input.checkpoint().await?;
        }
        control.check()?;
        construction::receive(output).await
    }
}

impl<'s: 'a, 'a> crate::plan::scalar::ScalarQueryInput<&'s Expr, &'s SelectStmt> for Arena<'a> {
    async fn parameter(
        &mut self,
        expression: &'s Expr,
        control: &ProductionControl<'_>,
    ) -> Result<Option<usize>> {
        control.check()?;
        Ok(self
            .value
            .copying
            .parameters
            .and_then(|parameters| parameters(expression)))
    }

    async fn append(
        &mut self,
        query: &'s SelectStmt,
        control: &ProductionControl<'_>,
    ) -> Result<usize> {
        self.work.checkpoint().await;
        control.check()?;
        self.value.queries.reserve(1)?;
        let index = self.value.queries.len();
        let mut item = self.value.copying.begin_input_field()?;
        let query = child(
            construct::Source::Select(query),
            &mut item,
            &mut self.work,
            self.aggregates,
        )
        .await?;
        self.value.copying.finish_input_field("item", item);
        self.value
            .queries
            .push(query.take())
            .expect("admitted nested query slot");
        Ok(index)
    }
}

async fn child<'s: 'a, 'c: 'a, 'a>(
    source: construct::Source<'s>,
    copying: &mut Copying<'c>,
    work: &mut Work<'a>,
    aggregates: &'a (dyn AggregateClassifier + Sync),
) -> Result<Prefix<QueryPlan>> {
    let field = copying.begin_input_field()?;
    let output =
        work.submit(move |work| async move { build(source, field, work, aggregates)?.await })?;
    let query = construction::receive(output).await?;
    let (query, memory) = query.into_query_parts();
    copying.finish_input_memory("query", memory);
    Ok(query)
}

fn build<'s: 'a, 'c: 'a, 'a>(
    source: construct::Source<'s>,
    mut copying: Copying<'c>,
    work: Work<'a>,
    aggregates: &'a (dyn AggregateClassifier + Sync),
) -> Result<construction::Read<'a, Result<Prefix<MutableQueryPlan>>>> {
    Ok(construction::Read::new(copying.control, async move {
        let control = copying.control;
        let query = {
            let mut scope = copying.enter_plan_node()?;
            let mut input = Native {
                aggregates,
                queries: Some(Arena::new(&scope, work.clone(), aggregates)),
                work: work.clone(),
            };
            let mut query = scope.retain_plan(crate::plan::retirement::empty_query());
            query_input(source, &mut scope, &mut input, &mut query)?.await?;
            query
        };
        query
            .map(|query| {
                MutableQueryPlan::from_rewrite_parts(query, Some(copying.into_rewrite_memory()))
            })
            .normalize_local_window_definitions_from(&control, work)?
            .await
    })?)
}

fn query_input<'s: 'a, 'c: 'a, 'a, I: construct::Input<'s, 'c> + Send + 'a>(
    source: construct::Source<'s>,
    copying: &'a mut Copying<'c>,
    input: &'a mut I,
    query: &'a mut Prefix<QueryPlan>,
) -> Result<construction::Read<'a, Result<()>>> {
    Ok(construction::Read::new(
        copying.control,
        construct::construct(source, copying, input, query),
    )?)
}

impl<'s: 'a, 'c: 'a, 'a> construct::Input<'s, 'c> for Native<'a, Arena<'a>> {
    async fn ctes(
        &mut self,
        source: &'s [crate::ast::CTE],
        copying: &mut Copying<'c>,
    ) -> Result<Prefix<Vec<CtePlan>>> {
        ctes::list_input(source, copying, self)?.await
    }

    async fn query(
        &mut self,
        source: construct::Source<'s>,
        copying: &mut Copying<'c>,
    ) -> Result<Prefix<QueryPlan>> {
        child(source, copying, &mut self.work, self.aggregates).await
    }
}

impl<'s: 'a, 'c: 'a, 'a> block::Input<'s, 'c> for Native<'a, Arena<'a>> {
    async fn source(
        &mut self,
        source: &'s crate::ast::FromClause,
        copying: &mut Copying<'c>,
    ) -> Result<Prefix<SourcePlan>> {
        trees::source_child(source, copying, self).await
    }

    async fn finish_arena(&mut self, copying: &mut Copying<'c>) -> Result<Prefix<Vec<QueryPlan>>> {
        self.work.checkpoint().await;
        let queries =
            copying.take_arena(self.queries.take().expect("completed query arena").value)?;
        Ok(copying.retain_plan(queries))
    }

    async fn compute(
        &mut self,
        block: &QueryBlockPlan,
        control: &ProductionControl<'c>,
    ) -> Result<ComputePlan> {
        block
            .compute_with_work(control, &mut self.work, &mut |expression| {
                crate::semantics::aggregates::is_aggregate_with_control(
                    self.aggregates,
                    expression,
                    control,
                )
            })
            .await
    }
}
