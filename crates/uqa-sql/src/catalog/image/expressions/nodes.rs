//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar syntax retains structural operation tags, original names and child order.

use super::{invalid, DefinitionImageReader, Frame, SQLError};
use crate::ast::{BinaryOp, Expr, FunctionCallSyntax};

impl<'a> DefinitionImageReader<'a> {
    pub(super) fn node(&mut self, value: &'a Expr) -> Result<(), SQLError> {
        if self.statement_expressions && is_query_expression(value) {
            return self.push(Frame::Plan(super::plans::Frame::Node(value, 0)));
        }
        self.fields.scalar([tag(value)?]);
        match value {
            Expr::Star | Expr::Default => {}
            Expr::QualifiedStar(name) | Expr::Column(name) => self.fields.text(name),
            Expr::QualifiedColumn { qualifier, column } => {
                self.fields.text(qualifier);
                self.fields.text(column);
            }
            Expr::InternalColumn(column) => {
                self.fields.scalar(column.relation().raw().to_le_bytes());
                self.fields
                    .scalar((column.attribute() as u32).to_le_bytes());
            }
            Expr::Literal(value) => self.push(Frame::Value(value))?,
            Expr::TypedLiteral {
                value,
                ty,
                collation_oid,
                composite_source,
            } => {
                self.fields.text(ty);
                self.fields.optional_u32(*collation_oid);
                self.fields.flag(composite_source.is_some());
                if let Some(source) = composite_source {
                    self.push(Frame::CompositeSource(source))?;
                }
                self.push(Frame::Value(value))?;
            }
            Expr::Param(index) => self.fields.count(*index),
            Expr::Func { .. } => self.function(value)?,
            Expr::Array(values) | Expr::Row(values) | Expr::And(values) | Expr::Or(values) => {
                self.fields.count(values.len());
                self.push(Frame::Nodes(values))?;
            }
            Expr::CompositeRow { items, binding } => {
                self.fields.count(items.len());
                self.push(Frame::Nodes(items))?;
                self.row_binding(binding)?;
            }
            Expr::Binary { op, lhs, rhs } => {
                self.fields.scalar([binary_tag(*op)]);
                self.push(Frame::Node(rhs))?;
                self.push(Frame::Node(lhs))?;
            }
            Expr::UnaryMinus(inner) | Expr::Not(inner) => self.push(Frame::Node(inner))?,
            Expr::IsNull { expr, negated } => {
                self.fields.flag(*negated);
                self.push(Frame::Node(expr))?;
            }
            Expr::Between { expr, low, high } => {
                self.push(Frame::Node(high))?;
                self.push(Frame::Node(low))?;
                self.push(Frame::Node(expr))?;
            }
            Expr::InList {
                expr,
                list,
                negated,
            } => {
                self.fields.flag(*negated);
                self.fields.count(list.len());
                self.push(Frame::Nodes(list))?;
                self.push(Frame::Node(expr))?;
            }
            Expr::Case {
                base,
                when,
                else_branch,
            } => {
                self.fields.flag(base.is_some());
                self.fields.count(when.len());
                self.fields.flag(else_branch.is_some());
                self.optional_node(else_branch.as_deref())?;
                self.push(Frame::Cases(when))?;
                self.optional_node(base.as_deref())?;
            }
            Expr::Cast { implicit, expr, ty } => {
                self.fields.flag(*implicit);
                self.fields.text(ty);
                self.push(Frame::Node(expr))?;
            }
            Expr::RetainedLiteral(value) => self.retained_literal(value)?,
            Expr::WindowCall { .. }
            | Expr::ScalarSubquery(_)
            | Expr::Exists { .. }
            | Expr::InSubquery { .. } => {
                unreachable!("query expressions have no stored-scalar image tag")
            }
        }
        Ok(())
    }

    fn retained_literal(
        &mut self,
        value: &'a crate::ast::RetainedInputLiteral,
    ) -> Result<(), SQLError> {
        self.retained_scalar_literal(&value.0)
    }

    pub(super) fn retained_scalar_literal(
        &mut self,
        literal: &'a crate::ir::RetainedScalarLiteral,
    ) -> Result<(), SQLError> {
        let resident = literal
            .value
            .resident_value()
            .ok_or_else(|| invalid("uncaptured stored literal"))?;
        self.fields.optional_text(literal.declaration.as_deref());
        self.fields.optional_u32(literal.collation_oid);
        self.fields.flag(literal.ty.is_some());
        self.push(Frame::Value(resident))?;
        if let Some(ty) = &literal.ty {
            self.push(Frame::Type(ty))?;
        }
        Ok(())
    }

    fn function(&mut self, value: &'a Expr) -> Result<(), SQLError> {
        let Expr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            order_syntax,
            filter,
        } = value
        else {
            unreachable!("function image node")
        };
        self.fields.text(name);
        self.fields.flag(binding.is_some());
        self.fields.flag(*distinct);
        self.fields.scalar([match order_syntax {
            FunctionCallSyntax::Legacy => 0,
            FunctionCallSyntax::Ordinary => 1,
            FunctionCallSyntax::WithinGroup => 2,
            FunctionCallSyntax::Extract => 3,
        }]);
        self.fields.count(args.len());
        self.fields.count(order_by.len());
        self.fields.flag(filter.is_some());
        self.optional_node(filter.as_deref())?;
        self.push(Frame::Orders(order_by))?;
        self.push(Frame::Nodes(args))?;
        if let Some(binding) = binding {
            self.push(Frame::Binding(binding))?;
        }
        Ok(())
    }
}

fn is_query_expression(value: &Expr) -> bool {
    matches!(
        value,
        Expr::ScalarSubquery(_)
            | Expr::Exists { .. }
            | Expr::InSubquery { .. }
            | Expr::WindowCall { .. }
    )
}

fn tag(value: &Expr) -> Result<u8, SQLError> {
    Ok(match value {
        Expr::Star => 0,
        Expr::QualifiedStar(_) => 1,
        Expr::Default => 2,
        Expr::Column(_) => 3,
        Expr::QualifiedColumn { .. } => 4,
        Expr::InternalColumn(_) => 5,
        Expr::Literal(_) => 6,
        Expr::TypedLiteral { .. } => 7,
        Expr::Param(_) => 8,
        Expr::Func { .. } => 9,
        Expr::Array(_) => 10,
        Expr::Row(_) => 11,
        Expr::CompositeRow { .. } => 12,
        Expr::Binary { .. } => 13,
        Expr::UnaryMinus(_) => 14,
        Expr::Not(_) => 15,
        Expr::And(_) => 16,
        Expr::Or(_) => 17,
        Expr::IsNull { .. } => 18,
        Expr::Between { .. } => 19,
        Expr::InList { .. } => 20,
        Expr::Case { .. } => 21,
        Expr::Cast { .. } => 22,
        Expr::RetainedLiteral(_) => 23,
        Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => return Err(invalid("query or window in stored scalar image")),
    })
}

pub(super) fn binary_tag(value: BinaryOp) -> u8 {
    match value {
        BinaryOp::Equal => 0,
        BinaryOp::NotEqual => 1,
        BinaryOp::Less => 2,
        BinaryOp::LessEqual => 3,
        BinaryOp::Greater => 4,
        BinaryOp::GreaterEqual => 5,
        BinaryOp::Add => 6,
        BinaryOp::Subtract => 7,
        BinaryOp::Multiply => 8,
        BinaryOp::Divide => 9,
    }
}
