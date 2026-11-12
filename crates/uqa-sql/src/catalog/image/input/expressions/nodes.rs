//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Headers construct native metadata and admit empty destinations before children arrive.

use super::{boxed, invalid, payload, Expr, Fields, MemoryReservation, SQLError};
use crate::ast::{BinaryOp, FunctionCallSyntax, InternalColumnRef, OrderBy};

impl Fields<'_> {
    pub(super) async fn expression_node(
        &mut self,
        memory: &mut MemoryReservation,
        value: &mut Expr,
    ) -> Result<(), SQLError> {
        self.binary.cancellation.check()?;
        let tag = self.binary.array::<1>().await?[0];
        self.expression_tag(tag, memory, value).await
    }

    pub(in super::super) async fn expression_tag(
        &mut self,
        tag: u8,
        memory: &mut MemoryReservation,
        value: &mut Expr,
    ) -> Result<(), SQLError> {
        match tag {
            0..=5 | 8 => self.scalar_reference(tag, memory, value).await,
            6 => {
                *value = Expr::Literal(payload(self.scalar_value().await?, memory));
                Ok(())
            }
            7 => self.typed_literal(memory, value).await,
            9 => self.scalar_function(memory, value).await,
            10..=12 | 16 | 17 => self.scalar_collection(tag, memory, value).await,
            13..=15 | 18..=22 => self.scalar_operator(tag, memory, value).await,
            23 => self.retained_literal(memory, value).await,
            _ => Err(invalid("stored scalar node tag")),
        }
    }

    async fn scalar_reference(
        &mut self,
        tag: u8,
        memory: &mut MemoryReservation,
        value: &mut Expr,
    ) -> Result<(), SQLError> {
        *value = match tag {
            0 => Expr::Star,
            1 => Expr::QualifiedStar(payload(self.binary.text().await?, memory).into()),
            2 => Expr::Default,
            3 => Expr::Column(payload(self.binary.text().await?, memory).into()),
            4 => Expr::QualifiedColumn {
                qualifier: payload(self.binary.text().await?, memory),
                column: payload(self.binary.text().await?, memory).into(),
            },
            5 => Expr::InternalColumn(InternalColumnRef::from_raw(
                u64::from_le_bytes(self.binary.array().await?),
                u32::from_le_bytes(self.binary.array().await?),
            )),
            8 => Expr::Param(self.binary.count().await?),
            _ => unreachable!("stored reference tag"),
        };
        Ok(())
    }

    async fn typed_literal(
        &mut self,
        memory: &mut MemoryReservation,
        output: &mut Expr,
    ) -> Result<(), SQLError> {
        let ty = payload(self.binary.text().await?, memory);
        let collation_oid = self.optional_number().await?;
        let source = self.flag().await?;
        let value = payload(self.scalar_value().await?, memory);
        let composite_source = if source {
            Some(boxed(self.composite_source(memory).await?, memory)?)
        } else {
            None
        };
        *output = Expr::TypedLiteral {
            value,
            ty,
            collation_oid,
            composite_source,
        };
        Ok(())
    }

    async fn scalar_collection(
        &mut self,
        tag: u8,
        memory: &mut MemoryReservation,
        value: &mut Expr,
    ) -> Result<(), SQLError> {
        *value = match tag {
            10 | 11 | 16 | 17 => {
                let count = self.binary.count().await?;
                let values = self.scalar_slots(count, memory).await?;
                match tag {
                    10 => Expr::Array(values),
                    11 => Expr::Row(values),
                    16 => Expr::And(values),
                    _ => Expr::Or(values),
                }
            }
            12 => {
                let count = self.binary.count().await?;
                let binding = self.scalar_row_binding(memory).await?;
                Expr::CompositeRow {
                    items: self.scalar_slots(count, memory).await?,
                    binding,
                }
            }
            _ => unreachable!("stored collection tag"),
        };
        Ok(())
    }

    async fn scalar_operator(
        &mut self,
        tag: u8,
        memory: &mut MemoryReservation,
        value: &mut Expr,
    ) -> Result<(), SQLError> {
        if tag == 21 {
            return self.scalar_case(memory, value).await;
        }
        *value = match tag {
            13 => {
                let op = match self.binary.array::<1>().await?[0] {
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
                negated: self.flag().await?,
                expr: boxed(Expr::Star, memory)?,
            },
            19 => Expr::Between {
                expr: boxed(Expr::Star, memory)?,
                low: boxed(Expr::Star, memory)?,
                high: boxed(Expr::Star, memory)?,
            },
            20 => {
                let negated = self.flag().await?;
                let count = self.binary.count().await?;
                Expr::InList {
                    negated,
                    expr: boxed(Expr::Star, memory)?,
                    list: self.scalar_slots(count, memory).await?,
                }
            }
            22 => {
                let implicit = self.flag().await?;
                let ty = payload(self.binary.text().await?, memory);
                Expr::Cast {
                    implicit,
                    ty,
                    expr: boxed(Expr::Star, memory)?,
                }
            }
            _ => unreachable!("stored operator tag"),
        };
        Ok(())
    }

    async fn scalar_case(
        &mut self,
        memory: &mut MemoryReservation,
        value: &mut Expr,
    ) -> Result<(), SQLError> {
        let base = self
            .flag()
            .await?
            .then(|| boxed(Expr::Star, memory))
            .transpose()?;
        let count = self.binary.count().await?;
        let else_branch = self
            .flag()
            .await?
            .then(|| boxed(Expr::Star, memory))
            .transpose()?;
        *value = Expr::Case {
            base,
            when: self
                .slots(count, memory, || (Expr::Star, Expr::Star))
                .await?,
            else_branch,
        };
        Ok(())
    }

    async fn scalar_function(
        &mut self,
        memory: &mut MemoryReservation,
        value: &mut Expr,
    ) -> Result<(), SQLError> {
        let name = payload(self.binary.text().await?, memory);
        let bound = self.flag().await?;
        let distinct = self.flag().await?;
        let order_syntax = match self.binary.array::<1>().await?[0] {
            0 => FunctionCallSyntax::Legacy,
            1 => FunctionCallSyntax::Ordinary,
            2 => FunctionCallSyntax::WithinGroup,
            3 => FunctionCallSyntax::Extract,
            _ => return Err(invalid("function call syntax tag")),
        };
        let args = self.binary.count().await?;
        let orders = self.binary.count().await?;
        let filter = self
            .flag()
            .await?
            .then(|| boxed(Expr::Star, memory))
            .transpose()?;
        let binding = if bound {
            if self.header(super::super::super::FUNCTION_BINDING).await? != 1 {
                return Err(invalid("nested function binding count"));
            }
            Some(payload(self.function_binding().await?, memory))
        } else {
            None
        };
        *value = Expr::Func {
            name,
            binding,
            distinct,
            order_syntax,
            filter,
            args: self.scalar_slots(args, memory).await?,
            order_by: self
                .slots(orders, memory, || OrderBy {
                    expr: Expr::Star,
                    descending: false,
                    nulls: None,
                })
                .await?,
        };
        Ok(())
    }
}
