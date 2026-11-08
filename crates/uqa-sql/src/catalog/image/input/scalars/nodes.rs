//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Headers construct native metadata and admit empty destinations before children arrive.

use super::{boxed, invalid, payload, Input, MemoryReservation, SQLError, ScalarExpr};
use crate::{
    ast::{BinaryOp, FunctionCallSyntax, InternalColumnRef},
    ir::ScalarOrder,
};

impl Input<'_> {
    #[expect(clippy::too_many_lines, reason = "exhaustive native scalar wire tags")]
    pub(super) fn native_scalar_node(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<ScalarExpr, SQLError> {
        self.binary.cancellation.check()?;
        let tag = self.binary.array::<1>()?[0];
        Ok(match tag {
            0 => ScalarExpr::Star,
            1 => ScalarExpr::QualifiedStar(payload(self.binary.text()?, memory).into()),
            2 => ScalarExpr::Default,
            3 => ScalarExpr::Column(payload(self.binary.text()?, memory).into()),
            4 => ScalarExpr::QualifiedColumn {
                qualifier: payload(self.binary.text()?, memory),
                column: payload(self.binary.text()?, memory).into(),
            },
            5 => ScalarExpr::InternalColumn(InternalColumnRef::from_raw(
                u64::from_le_bytes(self.binary.array()?),
                u32::from_le_bytes(self.binary.array()?),
            )),
            6 => ScalarExpr::Literal(payload(self.scalar_value()?, memory)),
            7 => {
                let ty = payload(self.binary.text()?, memory);
                let collation_oid = self.optional_number()?;
                let source = self.flag()?;
                let bound = self.flag()?;
                let parameter_index = self.flag()?.then(|| self.binary.count()).transpose()?;
                let value = payload(self.scalar_value()?, memory);
                let bound_type = bound
                    .then(|| self.column_type())
                    .transpose()?
                    .map(|ty| payload(ty, memory));
                let composite_source = if source {
                    Some(boxed(self.composite_source(memory)?, memory)?)
                } else {
                    None
                };
                ScalarExpr::TypedLiteral {
                    value,
                    ty,
                    collation_oid,
                    composite_source,
                    bound_type,
                    parameter_index,
                }
            }
            8 => ScalarExpr::Param(self.binary.count()?),
            9 => self.native_scalar_function(memory)?,
            10 | 11 | 16 | 17 => {
                let count = self.binary.count()?;
                let values = self.native_scalar_slots(count, memory)?;
                match tag {
                    10 => ScalarExpr::Array(values),
                    11 => ScalarExpr::Row(values),
                    16 => ScalarExpr::And(values),
                    _ => ScalarExpr::Or(values),
                }
            }
            12 => {
                let count = self.binary.count()?;
                let bound = self.flag()?;
                let binding = self.scalar_row_binding(memory)?;
                let bound_type = bound
                    .then(|| self.column_type())
                    .transpose()?
                    .map(|ty| payload(ty, memory));
                ScalarExpr::CompositeRow {
                    items: self.native_scalar_slots(count, memory)?,
                    binding,
                    bound_type,
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
                ScalarExpr::Binary {
                    op,
                    lhs: boxed(ScalarExpr::Star, memory)?,
                    rhs: boxed(ScalarExpr::Star, memory)?,
                }
            }
            14 => ScalarExpr::UnaryMinus(boxed(ScalarExpr::Star, memory)?),
            15 => ScalarExpr::Not(boxed(ScalarExpr::Star, memory)?),
            18 => ScalarExpr::IsNull {
                negated: self.flag()?,
                expr: boxed(ScalarExpr::Star, memory)?,
            },
            19 => ScalarExpr::Between {
                expr: boxed(ScalarExpr::Star, memory)?,
                low: boxed(ScalarExpr::Star, memory)?,
                high: boxed(ScalarExpr::Star, memory)?,
            },
            20 => {
                let negated = self.flag()?;
                let count = self.binary.count()?;
                ScalarExpr::InList {
                    negated,
                    expr: boxed(ScalarExpr::Star, memory)?,
                    list: self.native_scalar_slots(count, memory)?,
                }
            }
            21 => {
                let base = self
                    .flag()?
                    .then(|| boxed(ScalarExpr::Star, memory))
                    .transpose()?;
                let count = self.binary.count()?;
                let else_branch = self
                    .flag()?
                    .then(|| boxed(ScalarExpr::Star, memory))
                    .transpose()?;
                ScalarExpr::Case {
                    base,
                    when: self.items(count, memory, |_, _| {
                        Ok((ScalarExpr::Star, ScalarExpr::Star))
                    })?,
                    else_branch,
                }
            }
            22 => {
                let implicit = self.flag()?;
                let ty = payload(self.binary.text()?, memory);
                ScalarExpr::Cast {
                    implicit,
                    ty,
                    expr: boxed(ScalarExpr::Star, memory)?,
                }
            }
            23 => ScalarExpr::RetainedLiteral(self.retained_scalar_data()?),
            24 => ScalarExpr::Position(self.binary.count()?),
            25 => {
                let collation = self.native_scalar_collation()?;
                let ty = payload(self.column_type()?, memory);
                ScalarExpr::Collated {
                    collation,
                    ty,
                    expr: boxed(ScalarExpr::Star, memory)?,
                }
            }
            26 => ScalarExpr::ScalarSubquery(self.binary.count()?),
            27 => ScalarExpr::Exists {
                subquery: self.binary.count()?,
                negated: self.flag()?,
            },
            28 => ScalarExpr::InSubquery {
                subquery: self.binary.count()?,
                negated: self.flag()?,
                expr: boxed(ScalarExpr::Star, memory)?,
            },
            29 => self.native_scalar_window(memory)?,
            _ => return Err(invalid("native scalar node tag")),
        })
    }

    fn native_scalar_function(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<ScalarExpr, SQLError> {
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
            .then(|| boxed(ScalarExpr::Star, memory))
            .transpose()?;
        let binding = if bound {
            if self.header(super::super::super::FUNCTION_BINDING)? != 1 {
                return Err(invalid("nested function binding count"));
            }
            Some(payload(self.function_binding()?, memory))
        } else {
            None
        };
        Ok(ScalarExpr::Func {
            name,
            binding,
            distinct,
            order_syntax,
            filter,
            args: self.native_scalar_slots(args, memory)?,
            order_by: self.items(orders, memory, |_, _| {
                Ok(ScalarOrder {
                    expr: ScalarExpr::Star,
                    descending: false,
                    nulls: None,
                })
            })?,
        })
    }
}
