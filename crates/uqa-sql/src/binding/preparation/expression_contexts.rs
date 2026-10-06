//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cast, CASE, and window coercion rules for ordered parameter analysis.

use super::{
    error, ColumnType, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr,
};
use crate::ast::BinaryOp;
use uqa_core::Value;

impl Preparation<'_> {
    pub(super) fn cast_expression(
        &mut self,
        expr: &ScalarExpr,
        ty: &str,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ColumnType, SQLError> {
        let target = self.type_name(ty)?;
        // Parse analysis converts an untyped literal with the enum's input function before any assignment checks.
        if let ScalarExpr::Literal(value @ (Value::Str(_) | Value::Null)) = expr {
            crate::expr::enums::fold_unknown_literal(self.routines.enum_labels(), value, &target)?;
        }
        let mut source =
            if let (ScalarExpr::Array(items), ColumnType::Array(element)) = (expr, &target) {
                let mut items = items
                    .iter()
                    .map(|item| {
                        if matches!(item, ScalarExpr::Array(_)) {
                            self.cast_expression(item, ty, input, subqueries)
                                .map(|ty| ExpressionType::resolved(Some(ty)))
                        } else {
                            self.expression(item, input, subqueries)
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                for item in &mut items {
                    let element = if matches!(item.ty, Some(ColumnType::Array(_))) {
                        &target
                    } else {
                        element.as_ref()
                    };
                    self.parameters.coerce_unknown(item, element)?;
                    if let Some(source) = &item.ty {
                        if !crate::type_resolution::explicit_type_compatible(source, element) {
                            return Err(error(
                                "42846",
                                format!(
                                    "cannot cast type {} to {}",
                                    source.sql_name(),
                                    element.sql_name()
                                ),
                            ));
                        }
                    }
                }
                ExpressionType::resolved(Some(target.clone()))
            } else {
                self.expression(expr, input, subqueries)?
            };
        self.parameters.coerce_unknown(&mut source, &target)?;
        if source.ty.as_ref().is_some_and(|source| {
            !crate::type_resolution::explicit_type_compatible(source, &target)
        }) {
            return Err(error(
                "42846",
                format!(
                    "cannot cast type {} to {}",
                    source.ty.as_ref().expect("known source").sql_name(),
                    target.sql_name()
                ),
            ));
        }
        Ok(target)
    }

    pub(super) fn case_expression(
        &mut self,
        base: Option<&ScalarExpr>,
        when: &[(ScalarExpr, ScalarExpr)],
        else_branch: Option<&ScalarExpr>,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<Option<ColumnType>, SQLError> {
        let mut base = base
            .map(|base| self.expression(base, input, subqueries))
            .transpose()?;
        if let Some(base) = &mut base {
            self.parameters.coerce_unknown(base, &ColumnType::Text)?;
        }
        let mut values = Vec::new();
        for (condition, value) in when {
            if let Some(base) = &mut base {
                let mut condition = self.expression(condition, input, subqueries)?;
                self.binary(BinaryOp::Equal, base, &mut condition)?;
            } else {
                self.require_boolean(condition, input, subqueries, "CASE/WHEN")?;
            }
            values.push(self.expression(value, input, subqueries)?);
        }
        if let Some(other) = else_branch {
            values.insert(0, self.expression(other, input, subqueries)?);
        }
        self.common(crate::type_resolution::CommonTypeContext::Case, &mut values)
    }

    pub(super) fn window_specification(
        &mut self,
        spec: &crate::ScalarWindowSpec,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        let mut order_type = None;
        for item in &spec.order_by {
            let mut value = self.expression(&item.expr, input, subqueries)?;
            self.parameters
                .coerce_unknown(&mut value, &ColumnType::Text)?;
            if order_type.is_none() {
                order_type = value.ty;
            }
        }
        for item in &spec.partition_by {
            let mut value = self.expression(item, input, subqueries)?;
            self.parameters
                .coerce_unknown(&mut value, &ColumnType::Text)?;
        }
        if let Some(frame) = &spec.frame {
            // An untyped offset takes the type an `unknown` literal would: `bigint` for `ROWS` and `GROUPS`, and for `RANGE` the offset type of the ordering column's `in_range` support. A `RANGE` frame without exactly one ordering column is rejected when the query is analyzed.
            let target = match frame.mode {
                crate::ast::FrameMode::Range if spec.order_by.len() == 1 => {
                    Some(crate::range_frame_offset_type(order_type.as_ref(), None)?)
                }
                crate::ast::FrameMode::Range => None,
                crate::ast::FrameMode::Rows | crate::ast::FrameMode::Groups => {
                    Some(ColumnType::BigInteger)
                }
            };
            for bound in [&frame.start, &frame.end] {
                if let crate::ScalarFrameBound::Preceding(value)
                | crate::ScalarFrameBound::Following(value) = bound
                {
                    let mut value = self.expression(value, input, subqueries)?;
                    if let Some(target) = &target {
                        self.parameters.coerce_unknown(&mut value, target)?;
                    }
                }
            }
        }
        Ok(())
    }
}
