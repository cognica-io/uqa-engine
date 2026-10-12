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
use crate::catalog::collations::derivation::Derivation;
use uqa_core::Value;

impl Preparation<'_> {
    pub(super) fn cast_expression(
        &mut self,
        expr: &ScalarExpr,
        ty: &str,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let target = self.type_name(ty)?;
        self.cast_to_type(expr, &target, input, subqueries)
    }

    fn cast_to_type(
        &mut self,
        expr: &ScalarExpr,
        target: &ColumnType,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        // Parse analysis converts an untyped literal with the enum's input function before any assignment checks.
        if let ScalarExpr::Literal(value @ (Value::Str(_) | Value::Null)) = expr {
            crate::expr::enums::fold_unknown_literal(self.routines.enum_labels(), value, target)?;
        }
        let array_type = crate::type_resolution::base_type(target);
        let mut source = if let (ScalarExpr::Array(expressions), ColumnType::Array(element)) =
            (expr, array_type)
        {
            let mut items = expressions
                .iter()
                .map(|item| {
                    if matches!(item, ScalarExpr::Array(_)) {
                        self.cast_to_type(item, array_type, input, subqueries)
                    } else {
                        self.expression(item, input, subqueries)
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            for (expression, item) in expressions.iter().zip(&mut items) {
                let element = if matches!(item.ty, Some(ColumnType::Array(_))) {
                    array_type
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
                                source.display_name(),
                                element.display_name()
                            ),
                        ));
                    }
                }
                item.derivation = cast_collation(expression, item, element)?;
            }
            ExpressionType::combined(Some(array_type.clone()), &items)
        } else {
            self.expression(expr, input, subqueries)?
        };
        if let Some(fields) = &mut source.row_constructor {
            self.coerce_row_constructor(fields, target)?;
        }
        self.parameters.coerce_unknown(&mut source, target)?;
        if source
            .ty
            .as_ref()
            .is_some_and(|source| !crate::type_resolution::explicit_type_compatible(source, target))
        {
            return Err(error(
                "42846",
                format!(
                    "cannot cast type {} to {}",
                    source.ty.as_ref().expect("known source").display_name(),
                    target.display_name()
                ),
            ));
        }
        if source
            .ty
            .as_ref()
            .is_some_and(|source| crate::type_resolution::preserves_record_identity(source, target))
        {
            return Ok(source);
        }
        let mut result = ExpressionType::resolved(Some(target.clone()));
        result.derivation = cast_collation(expr, &source, target)?;
        if *target == ColumnType::Record {
            result.record_fields = source.record_fields;
            result.row_constructor = source.row_constructor;
        }
        Ok(result)
    }

    pub(super) fn case_expression(
        &mut self,
        base: Option<&ScalarExpr>,
        when: &[(ScalarExpr, ScalarExpr)],
        else_branch: Option<&ScalarExpr>,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let mut base = base
            .map(|base| self.expression(base, input, subqueries))
            .transpose()?;
        if let Some(base) = &mut base {
            self.parameters.coerce_unknown(base, &ColumnType::Text)?;
        }
        let base_error = base
            .as_ref()
            .map_or(Derivation::None, |value| value.derivation.error_only());
        let mut conditions = Vec::new();
        let mut values = Vec::new();
        for (index, (condition, value)) in when.iter().enumerate() {
            let selected = if let Some(base) = &mut base {
                let mut condition = self.expression(condition, input, subqueries)?;
                self.binary(BinaryOp::Equal, base, &mut condition)?;
                base.derivation.combine(condition.derivation).error_only()
            } else {
                self.boolean_expression(condition, input, subqueries, "CASE/WHEN")?
                    .derivation
                    .error_only()
            };
            if matches!(selected, Derivation::InvalidExplicit(..)) {
                conditions.push((index, selected));
            }
            values.push(self.expression(value, input, subqueries)?);
        }
        if let Some(other) = else_branch {
            values.insert(0, self.expression(other, input, subqueries)?);
        }
        let ty = self.common(crate::type_resolution::CommonTypeContext::Case, &mut values)?;
        // ELSE participates first in common-type selection but last in collation assignment.
        if else_branch.is_some() {
            values.rotate_left(1);
        }
        let mut conditions = conditions.into_iter().peekable();
        let mut selected = base_error;
        for (index, value) in values.iter().enumerate() {
            if conditions
                .peek()
                .is_some_and(|(position, _)| *position == index)
            {
                selected = selected.combine(conditions.next().unwrap().1);
            }
            selected = selected.combine(value.derivation);
        }
        let mut result = ExpressionType::resolved(ty);
        result.derivation = selected.result(result.derivation.oid());
        Ok(result)
    }

    pub(super) fn window_specification(
        &mut self,
        spec: &crate::ScalarWindowSpec,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        self.window_definition(spec, input, subqueries, None)
            .map(|_| ())
    }

    pub(super) fn window_definition(
        &mut self,
        spec: &crate::ScalarWindowSpec,
        input: &RowSchema,
        subqueries: &[QueryPlan],
        inherited: Option<&(usize, Option<ColumnType>)>,
    ) -> Result<(usize, Option<ColumnType>), SQLError> {
        let mut order_type = None;
        for item in &spec.order_by {
            let mut value = self.expression(&item.expr, input, subqueries)?;
            self.parameters
                .coerce_unknown(&mut value, &ColumnType::Text)?;
            if let Some(ty) = &value.ty {
                crate::type_resolution::ComparisonSupport::Ordering
                    .require_in_catalog(ty, self.routines)?;
            }
            if order_type.is_none() {
                order_type = value.ty;
            }
        }
        for item in &spec.partition_by {
            let mut value = self.expression(item, input, subqueries)?;
            self.parameters
                .coerce_unknown(&mut value, &ColumnType::Text)?;
            if let Some(ty) = &value.ty {
                crate::type_resolution::ComparisonSupport::Equality
                    .require_in_catalog(ty, self.routines)?;
            }
        }
        let (order_count, order_type) = if spec.order_by.is_empty() {
            inherited.cloned().unwrap_or_default()
        } else {
            (spec.order_by.len(), order_type)
        };
        if let Some(frame) = &spec.frame {
            // An untyped offset takes the type an `unknown` literal would: `bigint` for `ROWS` and `GROUPS`, and for `RANGE` the offset type of the ordering column's `in_range` support. A `RANGE` frame without exactly one ordering column is rejected when the query is analyzed.
            for bound in [&frame.start, &frame.end] {
                if let crate::ScalarFrameBound::Preceding(value)
                | crate::ScalarFrameBound::Following(value) = bound
                {
                    let target = match frame.mode {
                        crate::ast::FrameMode::Range if order_count == 1 => {
                            Some(crate::range_frame_offset_type(order_type.as_ref(), None)?)
                        }
                        crate::ast::FrameMode::Range => None,
                        crate::ast::FrameMode::Rows | crate::ast::FrameMode::Groups => {
                            Some(ColumnType::BigInteger)
                        }
                    };
                    let mut value = self.expression(value, input, subqueries)?;
                    if let Some(target) = &target {
                        self.parameters.coerce_unknown(&mut value, target)?;
                    }
                }
            }
        }
        Ok((order_count, order_type))
    }
}

fn cast_collation(
    expression: &ScalarExpr,
    source: &ExpressionType,
    target: &ColumnType,
) -> Result<Derivation, SQLError> {
    let selected = source.derivation.coerce(source.ty.as_ref(), target);
    // Coercion peels a direct COLLATE clause and reapplies it outside the converted value.
    if crate::catalog::type_metadata::pg_type_collation_oid(target) != 0
        && crate::catalog::collations::expression::scalar_clause(expression)?.is_some()
    {
        if let Derivation::Explicit(oid) = source.derivation {
            return Ok(selected.explicit(oid));
        }
    }
    Ok(selected)
}
