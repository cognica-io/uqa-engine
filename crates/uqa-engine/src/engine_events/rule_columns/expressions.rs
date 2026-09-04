//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expression traversal for durable rewrite-rule column binding.

use uqa_sql::ast::{Expr, FrameBound, OrderBy, WindowSpec};
use uqa_sql::SQLError;

use super::{same_identifier, ColumnBindingContext, ColumnScope, RuleColumnBinder};

impl RuleColumnBinder<'_> {
    pub(super) fn bind_expr(
        &mut self,
        expression: &mut Expr,
        scopes: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<(), SQLError> {
        match expression {
            Expr::Column(name) => {
                let stored_name = name.clone();
                let Some(matches) = scopes
                    .iter()
                    .map(|scope| scope.unqualified(&stored_name))
                    .find(|matches| !matches.is_empty())
                else {
                    return Ok(());
                };
                for column in &matches {
                    self.dependencies
                        .extend(column.dependencies.iter().cloned());
                }
                if let [column] = matches.as_slice() {
                    *expression = column.reference.clone();
                }
                Ok(())
            }
            Expr::QualifiedColumn { qualifier, column } => {
                let qualifier_name = qualifier.clone();
                let stored_column = column.clone();
                let Some(columns) = scopes
                    .iter()
                    .find_map(|scope| scope.qualified(&qualifier_name))
                else {
                    return Ok(());
                };
                let matches = columns
                    .iter()
                    .filter(|candidate| same_identifier(&candidate.name, &stored_column))
                    .collect::<Vec<_>>();
                for candidate in &matches {
                    self.dependencies
                        .extend(candidate.dependencies.iter().cloned());
                }
                if let [candidate] = matches.as_slice() {
                    column.clone_from(&candidate.current_name);
                }
                Ok(())
            }
            Expr::Func {
                args,
                order_by,
                filter,
                ..
            } => self.bind_function_parts(args, order_by, filter.as_deref_mut(), scopes, context),
            Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
                self.bind_exprs(items, scopes, context)
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.bind_expr(lhs, scopes, context)?;
                self.bind_expr(rhs, scopes, context)
            }
            Expr::UnaryMinus(inner)
            | Expr::Not(inner)
            | Expr::IsNull { expr: inner, .. }
            | Expr::Cast { expr: inner, .. } => self.bind_expr(inner, scopes, context),
            Expr::Between { expr, low, high } => {
                self.bind_expr(expr, scopes, context)?;
                self.bind_expr(low, scopes, context)?;
                self.bind_expr(high, scopes, context)
            }
            Expr::InList { expr, list, .. } => {
                self.bind_expr(expr, scopes, context)?;
                for item in list {
                    self.bind_expr(item, scopes, context)?;
                }
                Ok(())
            }
            Expr::WindowCall { args, spec, .. } => {
                self.bind_window_parts(args, spec, scopes, context)
            }
            Expr::Case {
                base,
                when,
                else_branch,
            } => self.bind_case_parts(
                base.as_deref_mut(),
                when,
                else_branch.as_deref_mut(),
                scopes,
                context,
            ),
            Expr::ScalarSubquery(body) | Expr::Exists { body, .. } => {
                self.bind_select(body, scopes, context)
            }
            Expr::InSubquery { expr, body, .. } => {
                self.bind_expr(expr, scopes, context)?;
                self.bind_select(body, scopes, context)
            }
            // Outside a projection list, `relation.*` is a whole-row value, so PostgreSQL keeps the composite reference live across column changes instead of recording one dependency per attribute.
            Expr::Star
            | Expr::QualifiedStar(_)
            | Expr::Default
            | Expr::InternalColumn(_)
            | Expr::Literal(_)
            | Expr::Param(_) => Ok(()),
        }
    }

    fn bind_exprs(
        &mut self,
        expressions: &mut [Expr],
        scopes: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<(), SQLError> {
        for expression in expressions {
            self.bind_expr(expression, scopes, context)?;
        }
        Ok(())
    }

    fn bind_function_parts(
        &mut self,
        args: &mut [Expr],
        order_by: &mut [OrderBy],
        filter: Option<&mut Expr>,
        scopes: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<(), SQLError> {
        for argument in args {
            self.bind_expr(argument, scopes, context)?;
        }
        for order in order_by {
            self.bind_expr(&mut order.expr, scopes, context)?;
        }
        if let Some(filter) = filter {
            self.bind_expr(filter, scopes, context)?;
        }
        Ok(())
    }

    fn bind_window_parts(
        &mut self,
        args: &mut [Expr],
        spec: &mut WindowSpec,
        scopes: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<(), SQLError> {
        for argument in args {
            self.bind_expr(argument, scopes, context)?;
        }
        for partition in &mut spec.partition_by {
            self.bind_expr(partition, scopes, context)?;
        }
        for order in &mut spec.order_by {
            self.bind_expr(&mut order.expr, scopes, context)?;
        }
        if let Some(frame) = &mut spec.frame {
            for bound in [&mut frame.start, &mut frame.end] {
                if let FrameBound::Preceding(inner) | FrameBound::Following(inner) = bound {
                    self.bind_expr(inner, scopes, context)?;
                }
            }
        }
        Ok(())
    }

    fn bind_case_parts(
        &mut self,
        base: Option<&mut Expr>,
        when: &mut [(Expr, Expr)],
        else_branch: Option<&mut Expr>,
        scopes: &[ColumnScope],
        context: &ColumnBindingContext,
    ) -> Result<(), SQLError> {
        if let Some(base) = base {
            self.bind_expr(base, scopes, context)?;
        }
        for (condition, result) in when {
            self.bind_expr(condition, scopes, context)?;
            self.bind_expr(result, scopes, context)?;
        }
        if let Some(branch) = else_branch {
            self.bind_expr(branch, scopes, context)?;
        }
        Ok(())
    }
}
