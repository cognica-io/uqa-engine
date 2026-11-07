//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Headers construct native metadata and admit empty destinations before children arrive.

use super::{boxed, invalid, payload, Expr, Input, MemoryReservation, SQLError};
use crate::ast::{BinaryOp, FunctionCallSyntax, InternalColumnRef, OrderBy};

impl Input<'_> {
    pub(super) fn expression_node(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<Expr, SQLError> {
        self.binary.cancellation.check()?;
        let tag = self.binary.array::<1>()?[0];
        self.expression_tag(tag, memory)
    }

    #[expect(clippy::too_many_lines, reason = "exhaustive stored scalar wire tags")]
    pub(in super::super) fn expression_tag(
        &mut self,
        tag: u8,
        memory: &mut MemoryReservation,
    ) -> Result<Expr, SQLError> {
        Ok(match tag {
            0 => Expr::Star,
            1 => Expr::QualifiedStar(payload(self.binary.text()?, memory).into()),
            2 => Expr::Default,
            3 => Expr::Column(payload(self.binary.text()?, memory).into()),
            4 => Expr::QualifiedColumn {
                qualifier: payload(self.binary.text()?, memory),
                column: payload(self.binary.text()?, memory).into(),
            },
            5 => Expr::InternalColumn(InternalColumnRef::from_raw(
                u64::from_le_bytes(self.binary.array()?),
                u32::from_le_bytes(self.binary.array()?),
            )),
            6 => Expr::Literal(payload(self.scalar_value()?, memory)),
            7 => {
                let ty = payload(self.binary.text()?, memory);
                let collation_oid = self.optional_number()?;
                let source = self.flag()?;
                let value = payload(self.scalar_value()?, memory);
                let composite_source = if source {
                    Some(boxed(self.composite_source(memory)?, memory)?)
                } else {
                    None
                };
                Expr::TypedLiteral {
                    value,
                    ty,
                    collation_oid,
                    composite_source,
                }
            }
            8 => Expr::Param(self.binary.count()?),
            9 => self.scalar_function(memory)?,
            10 | 11 | 16 | 17 => {
                let count = self.binary.count()?;
                let values = self.scalar_slots(count, memory)?;
                match tag {
                    10 => Expr::Array(values),
                    11 => Expr::Row(values),
                    16 => Expr::And(values),
                    _ => Expr::Or(values),
                }
            }
            12 => {
                let count = self.binary.count()?;
                let binding = self.scalar_row_binding(memory)?;
                Expr::CompositeRow {
                    items: self.scalar_slots(count, memory)?,
                    binding,
                }
            }
            13 => {
                let op = match self.binary.array::<1>()?[0] {
                    0 => BinaryOp::Equal,
                    1 => BinaryOp::NotEqual,
                    2 => BinaryOp::Less,
                    3 => BinaryOp::LessEqual,
                    4 => BinaryOp::Greater,
                    5 => BinaryOp::GreaterEqual,
                    6 => BinaryOp::Add,
                    7 => BinaryOp::Subtract,
                    8 => BinaryOp::Multiply,
                    9 => BinaryOp::Divide,
                    _ => return Err(invalid("scalar binary operator tag")),
                };
                Expr::Binary {
                    op,
                    lhs: boxed(Expr::Star, memory)?,
                    rhs: boxed(Expr::Star, memory)?,
                }
            }
            14 => Expr::UnaryMinus(boxed(Expr::Star, memory)?),
            15 => Expr::Not(boxed(Expr::Star, memory)?),
            18 => Expr::IsNull {
                negated: self.flag()?,
                expr: boxed(Expr::Star, memory)?,
            },
            19 => Expr::Between {
                expr: boxed(Expr::Star, memory)?,
                low: boxed(Expr::Star, memory)?,
                high: boxed(Expr::Star, memory)?,
            },
            20 => {
                let negated = self.flag()?;
                let count = self.binary.count()?;
                Expr::InList {
                    negated,
                    expr: boxed(Expr::Star, memory)?,
                    list: self.scalar_slots(count, memory)?,
                }
            }
            21 => {
                let base = self
                    .flag()?
                    .then(|| boxed(Expr::Star, memory))
                    .transpose()?;
                let count = self.binary.count()?;
                let else_branch = self
                    .flag()?
                    .then(|| boxed(Expr::Star, memory))
                    .transpose()?;
                Expr::Case {
                    base,
                    when: self.items(count, memory, |_, _| Ok((Expr::Star, Expr::Star)))?,
                    else_branch,
                }
            }
            22 => {
                let implicit = self.flag()?;
                let ty = payload(self.binary.text()?, memory);
                Expr::Cast {
                    implicit,
                    ty,
                    expr: boxed(Expr::Star, memory)?,
                }
            }
            23 => self.retained_literal(memory)?,
            _ => return Err(invalid("stored scalar node tag")),
        })
    }

    fn scalar_function(&mut self, memory: &mut MemoryReservation) -> Result<Expr, SQLError> {
        let name = payload(self.binary.text()?, memory);
        let bound = self.flag()?;
        let distinct = self.flag()?;
        let order_syntax = match self.binary.array::<1>()?[0] {
            0 => FunctionCallSyntax::Legacy,
            1 => FunctionCallSyntax::Ordinary,
            2 => FunctionCallSyntax::WithinGroup,
            3 => FunctionCallSyntax::Extract,
            _ => return Err(invalid("function call syntax tag")),
        };
        let args = self.binary.count()?;
        let orders = self.binary.count()?;
        let filter = self
            .flag()?
            .then(|| boxed(Expr::Star, memory))
            .transpose()?;
        let binding = if bound {
            if self.header(super::super::super::FUNCTION_BINDING)? != 1 {
                return Err(invalid("nested function binding count"));
            }
            Some(payload(self.function_binding()?, memory))
        } else {
            None
        };
        Ok(Expr::Func {
            name,
            binding,
            distinct,
            order_syntax,
            filter,
            args: self.scalar_slots(args, memory)?,
            order_by: self.items(orders, memory, |_, _| {
                Ok(OrderBy {
                    expr: Expr::Star,
                    descending: false,
                    nulls: None,
                })
            })?,
        })
    }
}
