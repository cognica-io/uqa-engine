//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{input::InlineScalarInput, Copying, QueryArena, Result};
use crate::{
    ast::FromClause,
    plan::{AggregateClassifier, JoinExecutionStrategy, SourcePlan},
};

pub(super) mod leaf;

impl Copying<'_> {
    pub(super) fn lower_source(
        &mut self,
        source: &FromClause,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<SourcePlan> {
        self.field("source", |copying| {
            copying.lower_source_fields(source, aggregates, arena)
        })
    }
    fn lower_source_fields(
        &mut self,
        source: &FromClause,
        aggregates: &dyn AggregateClassifier,
        arena: &mut QueryArena<'_>,
    ) -> Result<SourcePlan> {
        self.check()?;
        Ok(match source {
            FromClause::Join {
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
            } => SourcePlan::Join {
                comma: *comma,
                left: self.field("left", |copying| {
                    copying.boxed(|copying| copying.lower_source(left, aggregates, arena))
                })?,
                right: self.field("right", |copying| {
                    copying.boxed(|copying| copying.lower_source(right, aggregates, arena))
                })?,
                kind: *kind,
                on: self.field("on", |copying| {
                    copying.lower_optional_scalar(on.as_ref(), aggregates, arena)
                })?,
                using: using
                    .as_ref()
                    .map(|using| {
                        Ok::<_, crate::SQLError>(crate::ast::JoinUsing {
                            columns: self.texts(&using.columns)?,
                            alias: self.optional_text(using.alias.as_deref())?,
                        })
                    })
                    .transpose()?,
                natural: *natural,
                alias: self.optional_text(alias.as_deref())?,
                column_aliases: self.texts(column_aliases)?,
                lateral: *lateral,
                strategy: JoinExecutionStrategy::Auto,
            },
            FromClause::Subquery {
                lateral,
                body,
                alias,
                column_aliases,
            } => SourcePlan::Subquery {
                captured_output: None,
                lateral: *lateral,
                body: self.field("body", |copying| {
                    copying.boxed(|copying| copying.lower_nested_query(body, aggregates))
                })?,
                alias: self.optional_text(alias.as_deref())?,
                column_aliases: self.texts(column_aliases)?,
            },
            FromClause::RetainedRows(_)
            | FromClause::Table { .. }
            | FromClause::Values { .. }
            | FromClause::Function { .. }
            | FromClause::FunctionGroup { .. } => {
                return crate::catalog::work::inline(leaf::construct(
                    source,
                    self,
                    &mut InlineScalarInput { aggregates, arena },
                ))
                .map(crate::plan::retirement::Pending::take);
            }
        })
    }
}
