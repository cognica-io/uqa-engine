//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM trees share ordered child construction and their original query-local scalar arena.

use super::{
    expressions::construct_optional_scalar,
    input::{text, InlineScalarInput},
    Copying, QueryArena, Result,
};
use crate::{
    ast::{FromClause, SelectStmt},
    construction::{retain, Read},
    plan::{
        retirement::construction::Prefix, AggregateClassifier, JoinExecutionStrategy, QueryPlan,
        SourcePlan,
    },
};
use std::future::{ready, Future};

pub(super) mod leaf;

pub(super) trait Input<'s, 'c>: leaf::Input<'s> {
    fn source(
        &mut self,
        source: &'s FromClause,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<SourcePlan>>> + Send;
    fn query(
        &mut self,
        source: &'s SelectStmt,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<QueryPlan>>> + Send;
}

impl Copying<'_> {
    pub(super) fn lower_source(
        &mut self,
        source: &FromClause,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<SourcePlan> {
        let mut scope = self.enter_plan_node()?;
        scope.field("source", |copying| {
            crate::catalog::work::inline(construct(
                source,
                copying,
                &mut InlineScalarInput { aggregates, arena },
            ))
            .map(Prefix::take)
        })
    }
}

impl<'s, 'c> Input<'s, 'c> for InlineScalarInput<'_, '_> {
    fn source(
        &mut self,
        source: &'s FromClause,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<SourcePlan>>> + Send {
        ready(
            copying
                .lower_source(source, self.aggregates, self.arena)
                .map(|source| copying.retain_plan(source)),
        )
    }
    fn query(
        &mut self,
        source: &'s SelectStmt,
        copying: &mut Copying<'c>,
    ) -> impl Future<Output = Result<Prefix<QueryPlan>>> + Send {
        ready(
            copying
                .lower_nested_query(source, self.aggregates)
                .map(|query| copying.retain_plan(query)),
        )
    }
}

pub(super) async fn construct<'s, 'c, I: Input<'s, 'c>>(
    source: &'s FromClause,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<SourcePlan>> {
    copying.check()?;
    match source {
        FromClause::Join { .. } => retain(copying.control, join(source, copying, input))?.await,
        FromClause::Subquery { .. } => {
            retain(copying.control, subquery(source, copying, input))?.await
        }
        _ => {
            let source = retain(copying.control, leaf::construct(source, copying, input))?.await?;
            Ok(copying.retain_plan(source.take()))
        }
    }
}

async fn source_child<'s, 'c, I: Input<'s, 'c>>(
    source: &'s FromClause,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<Box<SourcePlan>>> {
    copying.admit_box::<SourcePlan>()?;
    let source = Read::new(copying.control, input.source(source, copying))?.await?;
    let source = copying.finish_box(source, Prefix::take)?;
    Ok(copying.retain_plan(source))
}

async fn join<'s, 'c, I: Input<'s, 'c>>(
    source: &'s FromClause,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<SourcePlan>> {
    let FromClause::Join {
        comma,
        left,
        right,
        kind,
        on,
        using,
        natural,
        alias,
        column_aliases,
        lateral,
    } = source
    else {
        unreachable!("join source")
    };
    input.checkpoint().await?;
    let left = field!(copying, "left", |field| source_child(
        left, &mut field, input
    )
    .await?);
    let right = field!(copying, "right", |field| source_child(
        right, &mut field, input
    )
    .await?);
    let on = field!(copying, "on", |field| construct_optional_scalar(
        on.as_ref(),
        &mut field,
        input
    )
    .await?);
    let using = match using {
        Some(using) => Some(crate::ast::JoinUsing {
            columns: text::texts(&using.columns, copying, input).await?,
            alias: text::optional_text(using.alias.as_deref(), copying, input).await?,
        }),
        None => None,
    };
    let alias = text::optional_text(alias.as_deref(), copying, input).await?;
    let column_aliases = text::texts(column_aliases, copying, input).await?;
    Ok(copying.retain_plan(SourcePlan::Join {
        comma: *comma,
        left: left.take(),
        right: right.take(),
        kind: *kind,
        on: on.take(),
        using,
        natural: *natural,
        alias,
        column_aliases,
        lateral: *lateral,
        strategy: JoinExecutionStrategy::Auto,
    }))
}

async fn subquery<'s, 'c, I: Input<'s, 'c>>(
    source: &'s FromClause,
    copying: &mut Copying<'c>,
    input: &mut I,
) -> Result<Prefix<SourcePlan>> {
    let FromClause::Subquery {
        lateral,
        body,
        alias,
        column_aliases,
    } = source
    else {
        unreachable!("derived query")
    };
    input.checkpoint().await?;
    let body = field!(copying, "body", |field| {
        field.admit_box::<QueryPlan>()?;
        let query = Read::new(field.control, input.query(body, &mut field))?.await?;
        let query = field.finish_box(query, Prefix::take)?;
        field.retain_plan(query)
    });
    let alias = text::optional_text(alias.as_deref(), copying, input).await?;
    let column_aliases = text::texts(column_aliases, copying, input).await?;
    Ok(copying.retain_plan(SourcePlan::Subquery {
        captured_output: None,
        lateral: *lateral,
        body: body.take(),
        alias,
        column_aliases,
    }))
}
