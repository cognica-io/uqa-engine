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
use crate::semantics::sets::placement::SetPlacement;
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
        let initial_collation = cast_initial_collation(expr, &source, target)?;
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
        result.initial_collation = initial_collation;
        if *target == ColumnType::Record {
            result.record_fields = source.record_fields;
            result.row_constructor = source.row_constructor;
            result.record_descriptor = source.record_descriptor;
            result.record_variable = source.record_variable;
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
        let calls = self.set_calls();
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
                // The test compares a placeholder for the base, so only the WHEN value can make it return a set.
                let before = self.set_calls();
                let mut condition = self.expression(condition, input, subqueries)?;
                self.binary(BinaryOp::Equal, base, &mut condition)?;
                self.reject_set_inputs(before, || {
                    crate::semantics::sets::placement::set_argument_error("CASE/WHEN")
                })?;
                base.derivation.combine(condition.derivation).error_only()
            } else {
                self.boolean_expression(condition, input, subqueries, "CASE/WHEN")?
                    .derivation
                    .error_only()
            };
            if selected.error_only() != Derivation::None {
                conditions.push((index, selected));
            }
            values.push(self.expression(value, input, subqueries)?);
        }
        values.insert(
            0,
            else_branch
                .map(|other| self.expression(other, input, subqueries))
                .transpose()?
                .unwrap_or_else(ExpressionType::unknown),
        );
        let ty = self.common(crate::type_resolution::CommonTypeContext::Case, &mut values)?;
        self.reject_set_inputs(calls, || {
            crate::semantics::sets::placement::conditional_set_error(
                "set-returning functions are not allowed in CASE",
            )
        })?;
        // ELSE participates first in common-type selection but last in collation assignment.
        values.rotate_left(1);
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
            self.parameters
                .collations
                .require_key(&item.expr, value.derivation);
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
            self.parameters
                .collations
                .require_key(item, value.derivation);
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
                    let mut value = self.in_placement(SetPlacement::WindowFrame, |analysis| {
                        analysis.expression(value, input, subqueries)
                    })?;
                    if let Some(target) = &target {
                        self.parameters.coerce_unknown(&mut value, target)?;
                    }
                }
            }
        }
        Ok((order_count, order_type))
    }
}

/// Parse-time tuple descriptors see leaf constants/parameters and identity casts before enclosing cast expressions receive their collations.
fn cast_initial_collation(
    expression: &ScalarExpr,
    source: &ExpressionType,
    target: &ColumnType,
) -> Result<u32, SQLError> {
    use crate::catalog::type_metadata::{pg_type_collation_oid, pg_type_modifier, pg_type_oid};
    let default = u32::try_from(pg_type_collation_oid(target)).expect("cast target collation OID");
    if default == 0 {
        return Ok(0);
    }
    if crate::catalog::collations::expression::scalar_clause(expression)?.is_some() {
        return Ok(source.initial_collation);
    }
    let modifier = pg_type_modifier(target);
    if source.ty.as_ref().is_some_and(|source| {
        pg_type_oid(source) == pg_type_oid(target)
            && (modifier < 0 || pg_type_modifier(source) == modifier)
    }) {
        return Ok(source.initial_collation);
    }
    if source.is_sql_unknown()
        && modifier < 0
        && (!matches!(target, ColumnType::Domain { .. }) || source.is_unknown_parameter())
    {
        return Ok(default);
    }
    Ok(0)
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
