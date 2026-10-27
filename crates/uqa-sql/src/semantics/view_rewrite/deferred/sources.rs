//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A source reads its preceding peers and lexical ancestors before publishing its own output names.

use super::{Cow, DeferredOuterRewrite, QueryBlockPlan, QueryPlan, RowSchema, SQLError};
use crate::binding::{SourceBindingInput, SourceScope};
use crate::plan::SourcePlan;

type BindSource<'a> = dyn FnMut(SourceBindingInput<'_>, &[QueryPlan], SourceScope<'_>) -> Result<RowSchema, SQLError>
    + 'a;

/// Bind view substitutions in source arguments and join conditions using the source's lexical input scope.
pub fn resolve_outer_source_rewrites<'a>(
    source: &'a SourcePlan,
    rewrites: &[DeferredOuterRewrite],
    subqueries: &mut Vec<QueryPlan>,
    scope: SourceScope<'_>,
    bind: &mut BindSource<'_>,
) -> Result<Cow<'a, SourcePlan>, SQLError> {
    if rewrites.is_empty() {
        return Ok(Cow::Borrowed(source));
    }
    let mut source = source.clone();
    rewrite_source(&mut source, rewrites, subqueries, scope, bind)?;
    Ok(Cow::Owned(source))
}

/// Prepare source-owned expressions independently of the projections that read the source's output.
pub fn resolve_outer_sources<'a>(
    block: &'a QueryBlockPlan,
    outer: Option<&RowSchema>,
    bind: &mut BindSource<'_>,
) -> Result<Cow<'a, QueryBlockPlan>, SQLError> {
    if block.deferred_outer_rewrites.is_empty() || block.from.is_none() {
        return Ok(Cow::Borrowed(block));
    }
    let mut block = block.clone();
    rewrite_source(
        block.from.as_mut().expect("source checked"),
        &block.deferred_outer_rewrites,
        &mut block.subqueries,
        SourceScope::new(outer),
        bind,
    )?;
    Ok(Cow::Owned(block))
}

fn rewrite_source(
    source: &mut SourcePlan,
    rewrites: &[DeferredOuterRewrite],
    subqueries: &mut Vec<QueryPlan>,
    scope: SourceScope<'_>,
    bind: &mut BindSource<'_>,
) -> Result<RowSchema, SQLError> {
    let local = SourceScope {
        outer: None,
        ..scope
    }
    .input(source)
    .unwrap_or_default();
    let inputs = match source {
        SourcePlan::Join {
            left,
            right,
            kind,
            lateral,
            on,
            ..
        } => {
            let left_schema = rewrite_source(left, rewrites, subqueries, scope.with_names(), bind)?;
            let preceding = scope.right_namespace(left, &left_schema, *kind);
            let right_schema = rewrite_source(
                right,
                rewrites,
                subqueries,
                SourceScope {
                    outer: scope.outer,
                    preceding: Some(&preceding),
                    inherited_lateral: *lateral,
                    retain_names: true,
                },
                bind,
            )?;
            if let Some(on) = on {
                let local = RowSchema::join(&left_schema, &right_schema, []);
                for rewrite in rewrites {
                    rewrite.in_namespace(&local).expression(on, subqueries);
                }
            }
            Some((left_schema, right_schema))
        }
        SourcePlan::Function { args, .. } => {
            for rewrite in rewrites {
                let rewrite = rewrite.in_namespace(&local);
                for expression in &mut *args {
                    rewrite.expression(expression, subqueries);
                }
            }
            None
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for rewrite in rewrites {
                let rewrite = rewrite.in_namespace(&local);
                for expression in functions.iter_mut().flat_map(|function| &mut function.args) {
                    rewrite.expression(expression, subqueries);
                }
            }
            None
        }
        SourcePlan::Values { rows, .. } => {
            for rewrite in rewrites {
                let rewrite = rewrite.in_namespace(&local);
                for expression in rows.iter_mut().flatten() {
                    rewrite.expression(expression, subqueries);
                }
            }
            None
        }
        SourcePlan::Subquery { body, .. } => {
            for rewrite in rewrites {
                rewrite.in_namespace(&local).query(body);
            }
            None
        }
        SourcePlan::RetainedRows(_) | SourcePlan::Table { .. } => None,
    };
    let input = inputs
        .as_ref()
        .map_or(SourceBindingInput::Leaf(source), |(left, right)| {
            SourceBindingInput::Join {
                source,
                left,
                right,
            }
        });
    bind(input, subqueries, scope)
}
