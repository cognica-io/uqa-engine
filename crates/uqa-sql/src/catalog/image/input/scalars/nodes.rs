//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Headers construct native metadata and admit empty destinations before children arrive.

use super::{boxed, invalid, payload, Fields, MemoryReservation, SQLError, ScalarExpr};
use crate::{
    ast::{BinaryOp, FunctionCallSyntax, InternalColumnRef},
    ir::ScalarOrder,
};

impl Fields<'_> {
    pub(in super::super) async fn native_scalar_tag(
        &mut self,
        tag: u8,
        memory: &mut MemoryReservation,
        value: &mut ScalarExpr,
    ) -> Result<(), SQLError> {
        match tag {
            0..=5 | 8 | 24 | 26 | 27 => self.native_reference(tag, memory, value).await,
            6 | 7 | 23 => self.native_literal(tag, memory, value).await,
            9 => self.native_scalar_function(memory, value).await,
            10..=12 | 16 | 17 => self.native_collection(tag, memory, value).await,
            13..=15 | 18..=22 | 25 | 28 => self.native_operator(tag, memory, value).await,
            29 => self.native_scalar_window(memory, value).await,
            _ => Err(invalid("native scalar node tag")),
        }
    }

    async fn native_literal(
        &mut self,
        tag: u8,
        memory: &mut MemoryReservation,
        value: &mut ScalarExpr,
    ) -> Result<(), SQLError> {
        match tag {
            6 => *value = ScalarExpr::Literal(payload(self.scalar_value().await?, memory)),
            7 => return self.native_typed_literal(memory, value).await,
            23 => *value = ScalarExpr::RetainedLiteral(self.retained_scalar_data().await?),
            _ => unreachable!("native scalar literal tag"),
        }
        Ok(())
    }

    async fn native_reference(
        &mut self,
        tag: u8,
        memory: &mut MemoryReservation,
        value: &mut ScalarExpr,
    ) -> Result<(), SQLError> {
        *value = match tag {
            0 => ScalarExpr::Star,
            1 => ScalarExpr::QualifiedStar(payload(self.binary.text().await?, memory).into()),
            2 => ScalarExpr::Default,
            3 => ScalarExpr::Column(payload(self.binary.text().await?, memory).into()),
            4 => ScalarExpr::QualifiedColumn {
                qualifier: payload(self.binary.text().await?, memory),
                column: payload(self.binary.text().await?, memory).into(),
            },
            5 => ScalarExpr::InternalColumn(InternalColumnRef::from_raw(
                u64::from_le_bytes(self.binary.array().await?),
                u32::from_le_bytes(self.binary.array().await?),
            )),
            8 => ScalarExpr::Param(self.binary.count().await?),
            24 => ScalarExpr::Position(self.binary.count().await?),
            26 => ScalarExpr::ScalarSubquery(self.binary.count().await?),
            27 => ScalarExpr::Exists {
                subquery: self.binary.count().await?,
                negated: self.flag().await?,
            },
            _ => unreachable!("stored reference tag"),
        };
        Ok(())
    }

    async fn native_typed_literal(
        &mut self,
        memory: &mut MemoryReservation,
        output: &mut ScalarExpr,
    ) -> Result<(), SQLError> {
        let ty = payload(self.binary.text().await?, memory);
        let collation_oid = self.optional_number().await?;
        let source = self.flag().await?;
        let bound = self.flag().await?;
        let parameter_index = if self.flag().await? {
            Some(self.binary.count().await?)
        } else {
            None
        };
        let value = payload(self.scalar_value().await?, memory);
        let bound_type = if bound {
            Some(payload(self.column_type().await?, memory))
        } else {
            None
        };
        let composite_source = if source {
            Some(boxed(self.composite_source(memory).await?, memory)?)
        } else {
            None
        };
        *output = ScalarExpr::TypedLiteral {
            value,
            ty,
            collation_oid,
            composite_source,
            bound_type,
            parameter_index,
        };
        Ok(())
    }

    async fn native_collection(
        &mut self,
        tag: u8,
        memory: &mut MemoryReservation,
        value: &mut ScalarExpr,
    ) -> Result<(), SQLError> {
        *value = match tag {
            10 | 11 | 16 | 17 => {
                let count = self.binary.count().await?;
                let values = self.native_scalar_slots(count, memory).await?;
                match tag {
                    10 => ScalarExpr::Array(values),
                    11 => ScalarExpr::Row(values),
                    16 => ScalarExpr::And(values),
                    _ => ScalarExpr::Or(values),
                }
            }
            12 => {
                let count = self.binary.count().await?;
                let bound = self.flag().await?;
                let binding = self.scalar_row_binding(memory).await?;
                let bound_type = if bound {
                    Some(payload(self.column_type().await?, memory))
                } else {
                    None
                };
                ScalarExpr::CompositeRow {
                    items: self.native_scalar_slots(count, memory).await?,
                    binding,
                    bound_type,
                }
            }
            _ => unreachable!("stored collection tag"),
        };
        Ok(())
    }

    async fn native_operator(
        &mut self,
        tag: u8,
        memory: &mut MemoryReservation,
        value: &mut ScalarExpr,
    ) -> Result<(), SQLError> {
        if tag == 21 {
            return self.native_case(memory, value).await;
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
                ScalarExpr::Binary {
                    op,
                    lhs: boxed(ScalarExpr::Star, memory)?,
                    rhs: boxed(ScalarExpr::Star, memory)?,
                }
            }
            14 => ScalarExpr::UnaryMinus(boxed(ScalarExpr::Star, memory)?),
            15 => ScalarExpr::Not(boxed(ScalarExpr::Star, memory)?),
            18 => ScalarExpr::IsNull {
                negated: self.flag().await?,
                expr: boxed(ScalarExpr::Star, memory)?,
            },
            19 => ScalarExpr::Between {
                expr: boxed(ScalarExpr::Star, memory)?,
                low: boxed(ScalarExpr::Star, memory)?,
                high: boxed(ScalarExpr::Star, memory)?,
            },
            20 => {
                let negated = self.flag().await?;
                let count = self.binary.count().await?;
                ScalarExpr::InList {
                    negated,
                    expr: boxed(ScalarExpr::Star, memory)?,
                    list: self.native_scalar_slots(count, memory).await?,
                }
            }
            22 => {
                let implicit = self.flag().await?;
                let ty = payload(self.binary.text().await?, memory);
                ScalarExpr::Cast {
                    implicit,
                    ty,
                    expr: boxed(ScalarExpr::Star, memory)?,
                }
            }
            25 => {
                let collation = self.native_scalar_collation().await?;
                let ty = payload(self.column_type().await?, memory);
                ScalarExpr::Collated {
                    collation,
                    ty,
                    expr: boxed(ScalarExpr::Star, memory)?,
                }
            }
            28 => ScalarExpr::InSubquery {
                subquery: self.binary.count().await?,
                negated: self.flag().await?,
                expr: boxed(ScalarExpr::Star, memory)?,
            },
            _ => unreachable!("native scalar operator tag"),
        };
        Ok(())
    }

    async fn native_case(
        &mut self,
        memory: &mut MemoryReservation,
        value: &mut ScalarExpr,
    ) -> Result<(), SQLError> {
        let base = self
            .flag()
            .await?
            .then(|| boxed(ScalarExpr::Star, memory))
            .transpose()?;
        let count = self.binary.count().await?;
        let else_branch = self
            .flag()
            .await?
            .then(|| boxed(ScalarExpr::Star, memory))
            .transpose()?;
        *value = ScalarExpr::Case {
            base,
            when: self
                .slots(count, memory, || (ScalarExpr::Star, ScalarExpr::Star))
                .await?,
            else_branch,
        };
        Ok(())
    }

    async fn native_scalar_function(
        &mut self,
        memory: &mut MemoryReservation,
        value: &mut ScalarExpr,
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
            .then(|| boxed(ScalarExpr::Star, memory))
            .transpose()?;
        let binding = if bound {
            if self.header(super::super::super::FUNCTION_BINDING).await? != 1 {
                return Err(invalid("nested function binding count"));
            }
            Some(payload(self.function_binding().await?, memory))
        } else {
            None
        };
        *value = ScalarExpr::Func {
            name,
            binding,
            distinct,
            order_syntax,
            filter,
            args: self.native_scalar_slots(args, memory).await?,
            order_by: self
                .slots(orders, memory, || ScalarOrder {
                    expr: ScalarExpr::Star,
                    descending: false,
                    nulls: None,
                })
                .await?,
        };
        Ok(())
    }
}
