//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-local window definitions and selected OVER references.

use crate::{ScalarFrameBound, ScalarWindowSpec};
use uqa_sql::ir::ScalarExpr;
use uqa_sql::plan::{QueryBlockPlan, QueryPlan};

use super::{quote_ident, Deparser, SQLError, Scope};

#[derive(Clone)]
pub(super) struct RenderedWindow {
    name: Option<String>,
    specification: String,
}

impl Deparser<'_> {
    pub(super) fn prepare_windows(
        &self,
        block: &QueryBlockPlan,
        scope: &mut Scope,
    ) -> Result<(), SQLError> {
        scope.windows.clear();
        for (position, definition) in block.windows.iter().enumerate() {
            let own = self.window(&definition.spec, scope, &block.subqueries)?;
            let specification = if let Some(inherited) = definition.inherited {
                let name = block
                    .windows
                    .get(inherited)
                    .filter(|_| inherited < position)
                    .and_then(|parent| parent.name.as_deref())
                    .ok_or_else(|| {
                        SQLError::Internal(
                            "stored window inheritance has no preceding named definition".into(),
                        )
                    })?;
                if own.is_empty() {
                    quote_ident(name)
                } else {
                    format!("{} {own}", quote_ident(name))
                }
            } else {
                own
            };
            scope.windows.push(RenderedWindow {
                name: definition.name.clone(),
                specification,
            });
        }
        Ok(())
    }

    pub(super) fn window_clause(&self, rendered: &mut String, scope: &Scope) {
        let declarations = scope
            .windows
            .iter()
            .filter_map(|definition| {
                definition
                    .name
                    .as_ref()
                    .map(|name| format!("{} AS ({})", quote_ident(name), definition.specification))
            })
            .collect::<Vec<_>>();
        if !declarations.is_empty() {
            self.clause(
                rendered,
                "  WINDOW ",
                &declarations.join(", "),
                scope.indent,
            );
        }
    }

    /// A window call as `get_windowfunc_expr` prints it: the call, its `FILTER`, and `OVER` with the window.
    pub(super) fn window_call(
        &self,
        name: &str,
        args: &[ScalarExpr],
        (filter, spec): (Option<&ScalarExpr>, &ScalarWindowSpec),
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let filter = filter
            .map(|filter| self.expression(filter, scope, subqueries))
            .transpose()?
            .map(|filter| format!(" FILTER (WHERE {filter})"))
            .unwrap_or_default();
        let window = if let Some(slot) = spec.definition {
            let definition = scope.windows.get(slot).ok_or_else(|| {
                SQLError::Internal("stored window call has no query-local definition".into())
            })?;
            definition.name.as_ref().map_or_else(
                || format!("({})", definition.specification),
                |name| quote_ident(name),
            )
        } else {
            format!("({})", self.window(spec, scope, subqueries)?)
        };
        Ok(format!(
            "{}{filter} OVER {window}",
            self.function(name, None, args, scope, subqueries)?,
        ))
    }

    fn window(
        &self,
        spec: &ScalarWindowSpec,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let mut parts = Vec::new();
        if !spec.partition_by.is_empty() {
            parts.push(format!(
                "PARTITION BY {}",
                self.expressions(&spec.partition_by, scope, subqueries)?
            ));
        }
        if !spec.order_by.is_empty() {
            let order = spec
                .order_by
                .iter()
                .map(|order| {
                    self.order_expression(
                        &order.expr,
                        order.descending,
                        order.nulls,
                        scope,
                        subqueries,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            parts.push(format!("ORDER BY {}", order.join(", ")));
        }
        if let Some(frame) = &spec.frame {
            parts.push(uqa_sql::render::frame_clause_sql(
                frame.mode,
                &self.frame_bound(&frame.start, scope, subqueries)?,
                &self.frame_bound(&frame.end, scope, subqueries)?,
                frame.between,
                frame.exclusion,
            ));
        }
        Ok(parts.join(" "))
    }

    fn frame_bound(
        &self,
        bound: &ScalarFrameBound,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        Ok(match bound {
            ScalarFrameBound::UnboundedPreceding => "UNBOUNDED PRECEDING".into(),
            ScalarFrameBound::UnboundedFollowing => "UNBOUNDED FOLLOWING".into(),
            ScalarFrameBound::CurrentRow => "CURRENT ROW".into(),
            ScalarFrameBound::Preceding(value) => {
                format!("{} PRECEDING", self.expression(value, scope, subqueries)?)
            }
            ScalarFrameBound::Following(value) => {
                format!("{} FOLLOWING", self.expression(value, scope, subqueries)?)
            }
        })
    }
}
