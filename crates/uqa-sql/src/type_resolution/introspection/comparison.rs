//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Numeric comparison inputs retain their selected SQL operator coercions before planning.

use super::{Binder, ColumnType, Produced, SQLError, ScalarExpr};
use crate::ast::BinaryOp;
use crate::type_resolution::{common::base_type, operators::binary_operator_types_with_control};

impl Binder<'_, '_> {
    pub(super) fn numeric_comparison_types(
        &self,
        op: BinaryOp,
        left: &ScalarExpr,
        right: &ScalarExpr,
    ) -> Result<Option<Produced<[ColumnType; 3]>>, SQLError> {
        if !matches!(
            op,
            BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Less
                | BinaryOp::LessEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterEqual
        ) {
            return Ok(None);
        }
        let left_type = self.common_context(left)?;
        let right_type = self.common_context(right)?;
        // A schema-less column is a dynamic carrier, not a PostgreSQL unknown literal. Its runtime value cannot be narrowed using only the other operand.
        if (left_type.is_none() && !unknown_input(left))
            || (right_type.is_none() && !unknown_input(right))
        {
            return Ok(None);
        }
        let left = left_type;
        let right = right_type;
        if !left
            .as_deref()
            .into_iter()
            .chain(right.as_deref())
            .any(|ty| numeric_type(base_type(ty)))
        {
            return Ok(None);
        }
        binary_operator_types_with_control(op, left.as_deref(), right.as_deref(), &self.control)
            .map(Some)
    }
}

fn unknown_input(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Literal(uqa_core::Value::Null | uqa_core::Value::Str(_)) | ScalarExpr::Param(_)
    )
}

fn numeric_type(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::SmallInteger
            | ColumnType::Integer
            | ColumnType::BigInteger
            | ColumnType::Real
            | ColumnType::DoublePrecision
            | ColumnType::Numeric { .. }
    )
}

#[cfg(test)]
mod tests;
