//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CatalogRetentionError, Node, Result, Walker};
use crate::ast::{Expr, FrameBound, OrderBy, WindowFrame, WindowReference, WindowSpec};

impl<'a> Walker<'a> {
    pub(super) fn expr(&mut self, expr: &'a Expr) -> Result<()> {
        match expr {
            Expr::Star | Expr::Default | Expr::InternalColumn(_) | Expr::Param(_) => {}
            Expr::QualifiedStar(name) | Expr::Column(name) => self.text(name)?,
            Expr::QualifiedColumn { qualifier, column } => {
                self.text(qualifier)?;
                self.text(column)?;
            }
            Expr::Literal(value) => self.value(value)?,
            Expr::TypedLiteral { value, ty } => {
                self.value(value)?;
                self.text(ty)?;
            }
            Expr::Func {
                name,
                binding,
                args,
                distinct: _,
                order_by,
                filter,
            } => {
                self.text(name)?;
                if let Some(binding) = binding {
                    self.node(Node::Binding(binding))?;
                }
                self.children(args, Node::Expr)?;
                self.orders(order_by)?;
                self.optional_boxed_expr(filter)?;
            }
            Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
                self.children(items, Node::Expr)?;
            }
            Expr::Binary { op: _, lhs, rhs } => {
                self.boxed(lhs.as_ref(), Node::Expr)?;
                self.boxed(rhs.as_ref(), Node::Expr)?;
            }
            Expr::UnaryMinus(expr) | Expr::Not(expr) | Expr::IsNull { expr, negated: _ } => {
                self.boxed(expr.as_ref(), Node::Expr)?;
            }
            Expr::Between { expr, low, high } => {
                self.boxed(expr.as_ref(), Node::Expr)?;
                self.boxed(low.as_ref(), Node::Expr)?;
                self.boxed(high.as_ref(), Node::Expr)?;
            }
            Expr::InList {
                expr,
                list,
                negated: _,
            } => {
                self.boxed(expr.as_ref(), Node::Expr)?;
                self.children(list, Node::Expr)?;
            }
            Expr::WindowCall { name, args, spec } => {
                self.text(name)?;
                self.children(args, Node::Expr)?;
                self.window(spec)?;
            }
            Expr::Case {
                base,
                when,
                else_branch,
            } => {
                self.optional_boxed_expr(base)?;
                self.buffer::<(Expr, Expr)>(when.capacity())?;
                for (condition, value) in when {
                    self.node(Node::Expr(condition))?;
                    self.node(Node::Expr(value))?;
                }
                self.optional_boxed_expr(else_branch)?;
            }
            Expr::Cast { expr, ty } => {
                self.boxed(expr.as_ref(), Node::Expr)?;
                self.text(ty)?;
            }
            // These shapes cannot be published by defaults::validate_default_expression, constraints::validate_check_expression or generated::prepare_generated_columns.
            Expr::ScalarSubquery(_)
            | Expr::Exists {
                body: _,
                negated: _,
            }
            | Expr::InSubquery {
                expr: _,
                body: _,
                negated: _,
            } => {
                return Err(CatalogRetentionError::UnexpectedSubquery);
            }
        }
        Ok(())
    }

    fn orders(&mut self, orders: &'a Vec<OrderBy>) -> Result<()> {
        self.buffer::<OrderBy>(orders.capacity())?;
        for OrderBy {
            expr,
            descending: _,
            nulls: _,
        } in orders
        {
            self.node(Node::Expr(expr))?;
        }
        Ok(())
    }

    fn window(&mut self, window: &'a WindowSpec) -> Result<()> {
        let WindowSpec {
            reference,
            partition_by,
            order_by,
            frame,
        } = window;
        if let Some(WindowReference { name, kind: _ }) = reference {
            self.text(name)?;
        }
        self.children(partition_by, Node::Expr)?;
        self.orders(order_by)?;
        if let Some(WindowFrame {
            mode: _,
            start,
            end,
        }) = frame
        {
            for bound in [start, end] {
                match bound {
                    FrameBound::UnboundedPreceding
                    | FrameBound::UnboundedFollowing
                    | FrameBound::CurrentRow => {}
                    FrameBound::Preceding(expr) | FrameBound::Following(expr) => {
                        self.boxed(expr.as_ref(), Node::Expr)?;
                    }
                }
            }
        }
        Ok(())
    }
}
