//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cast, CASE, and window coercion rules for ordered parameter analysis.

use super::{
    error, ColumnType, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr,
};
use uqa_sql::ast::BinaryOp;

impl Preparation<'_> {
    pub(super) fn cast_expression(
        &mut self,
        expr: &ScalarExpr,
        ty: &str,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ColumnType, SQLError> {
        let target = self.type_name(ty)?;
        let mut source = if let (ScalarExpr::Array(items), ColumnType::Array(element)) =
            (expr, &target)
        {
            let mut items = items
                .iter()
                .map(|item| self.expression(item, input, subqueries))
                .collect::<Result<Vec<_>, _>>()?;
            for item in &mut items {
                self.parameters.coerce_unknown(item, element)?;
                if let Some(source) = &item.ty {
                    if !uqa_execution::type_resolution::explicit_type_compatible(source, element) {
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
            !uqa_execution::type_resolution::explicit_type_compatible(source, &target)
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
    ) -> Result<ColumnType, SQLError> {
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
        self.common(&mut values)
    }

    pub(super) fn window_specification(
        &mut self,
        spec: &uqa_execution::ScalarWindowSpec,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        for item in &spec.partition_by {
            let mut value = self.expression(item, input, subqueries)?;
            self.parameters
                .coerce_unknown(&mut value, &ColumnType::Text)?;
        }
        let mut order_type = None;
        for item in &spec.order_by {
            let mut value = self.expression(&item.expr, input, subqueries)?;
            self.parameters
                .coerce_unknown(&mut value, &ColumnType::Text)?;
            if order_type.is_none() {
                order_type = value.ty;
            }
        }
        if let Some(frame) = &spec.frame {
            let target = if frame.mode == uqa_sql::ast::FrameMode::Range {
                match order_type
                    .as_ref()
                    .map(ColumnType::without_temporal_modifiers)
                {
                    Some(
                        ColumnType::Date
                        | ColumnType::Timestamp
                        | ColumnType::TimestampTz
                        | ColumnType::Time
                        | ColumnType::TimeTz
                        | ColumnType::Interval,
                    ) => ColumnType::Interval,
                    Some(ty) => ty.clone(),
                    None => ColumnType::BigInteger,
                }
            } else {
                ColumnType::BigInteger
            };
            for bound in [&frame.start, &frame.end] {
                if let uqa_execution::ScalarFrameBound::Preceding(value)
                | uqa_execution::ScalarFrameBound::Following(value) = bound
                {
                    let mut value = self.expression(value, input, subqueries)?;
                    self.parameters.coerce_unknown(&mut value, &target)?;
                }
            }
        }
        Ok(())
    }
}
