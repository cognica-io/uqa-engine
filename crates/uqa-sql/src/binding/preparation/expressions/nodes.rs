//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observe recursive nodes in separate frames so unrelated scalar variants do not accumulate stack storage at every child.

use super::{
    BinaryOp, ColumnType, Derivation, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError,
    ScalarExpr,
};

impl Preparation<'_> {
    pub(super) fn scalar_subquery_expression(
        &mut self,
        index: usize,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let schema = self.scalar_query_schema(index, input, subqueries)?;
        let control = self.scope.control();
        let mut result = ExpressionType::resolved_produced(
            schema
                .column_type(0)
                .map(|ty| ty.clone_with_control(&control))
                .transpose()?,
            &control,
        )?;
        result.initial_collation =
            u32::try_from(result.derivation.oid()).expect("type collation OID");
        if let Some(oid) = schema.column_collation(0) {
            result.derivation = Derivation::implicit(i64::from(oid));
            result.initial_collation = oid;
        }
        result.record_fields = schema.record_fields(0).cloned();
        Ok(result)
    }

    pub(super) fn collated_expression(
        &mut self,
        expr: &ScalarExpr,
        ty: &ColumnType,
        collation: crate::ir::ResultCollation,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        // The wrapped node has already been assigned. Analyze its inputs without scheduling another wrapper around that same node.
        let mut value = self.expression_inner(expr, input, subqueries)?;
        collation.validate(ty)?;
        // Collation assignment retains type identity, while a simplified arm keeps its own modifier.
        let control = self.scope.control();
        let differs = match value.ty.as_deref() {
            Some(inner) if inner == ty => false,
            Some(inner) => {
                *inner.without_type_modifiers_with_control(&control)?
                    != *ty.without_type_modifiers_with_control(&control)?
            }
            None => true,
        };
        if differs {
            value.replace_type(Some(ty.clone_with_control(&control)?), &control)?;
        }
        value.derivation = value
            .derivation
            .error_only()
            .combine(collation.derivation());
        value.initial_collation = collation.initial_oid();
        Ok(value)
    }

    pub(super) fn binary_expression(
        &mut self,
        op: BinaryOp,
        lhs: &ScalarExpr,
        rhs: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let calls = self.set_calls();
        let mut left = self.expression(lhs, input, subqueries)?;
        let mut right = self.expression(rhs, input, subqueries)?;
        if matches!((lhs, rhs), (ScalarExpr::Row(_), ScalarExpr::Row(_))) {
            self.coerce_row_comparison(op, &mut left, &mut right, false)?;
            self.reject_set_inputs(
                calls,
                crate::semantics::sets::placement::row_comparison_set_error,
            )?;
            let mut result = ExpressionType::resolved(Some(ColumnType::Boolean));
            result.derivation = ExpressionType::comparison(&left, &right);
            return Ok(result);
        }
        let ty = self.binary(op, &mut left, &mut right)?;
        Ok(ExpressionType::combined_owned(Some(ty), &[left, right]))
    }

    pub(super) fn not_expression(
        &mut self,
        inner: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let value = self.boolean_expression(inner, input, subqueries, "NOT")?;
        Ok(ExpressionType::independent(
            Some(ColumnType::Boolean),
            &[value],
        ))
    }

    pub(super) fn boolean_chain_expression(
        &mut self,
        items: &[ScalarExpr],
        context: &str,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let mut selected = Derivation::None;
        for item in items {
            let value = self.boolean_expression(item, input, subqueries, context)?;
            selected = selected.combine(value.derivation.error_only());
        }
        let mut result = ExpressionType::resolved(Some(ColumnType::Boolean));
        result.derivation = selected;
        Ok(result)
    }

    pub(super) fn null_test_expression(
        &mut self,
        expr: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let value = self.expression(expr, input, subqueries)?;
        Ok(ExpressionType::independent(
            Some(ColumnType::Boolean),
            &[value],
        ))
    }

    pub(super) fn unary_minus_expression(
        &mut self,
        expression: &ScalarExpr,
        inner: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let inner = self.expression(inner, input, subqueries)?;
        if inner.ty.is_none() {
            return Err(crate::type_resolution::ambiguous_prefix_operator(
                "-", "unknown",
            ));
        }
        let ty = self.known_type(expression, input, subqueries)?;
        Ok(ExpressionType::independent_owned(ty, &[inner]))
    }

    pub(super) fn array_expression(
        &mut self,
        items: &[ScalarExpr],
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let owner = self.scope.control().capture();
        let control = owner.control();
        let mut observed = super::super::ownership::AnalysisVec::new(&control);
        observed.reserve(items.len(), &control)?;
        for item in items {
            observed.push(self.expression(item, input, subqueries)?, &control)?;
        }
        let ty = self
            .common(
                crate::type_resolution::CommonTypeContext::Array,
                &mut observed,
            )?
            .map(|element| element.array(&control))
            .transpose()?;
        Ok(ExpressionType::combined_owned(ty, &observed))
    }

    pub(super) fn composite_row_expression(
        &mut self,
        items: &[ScalarExpr],
        binding: &crate::ast::CompositeRowBinding,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let mut selected = Derivation::None;
        for item in items {
            selected = selected.combine(
                self.expression(item, input, subqueries)?
                    .derivation
                    .error_only(),
            );
        }
        let mut result = ExpressionType::resolved_owned(Some(self.type_name(&binding.ty)?));
        result.derivation = selected;
        Ok(result)
    }

    pub(super) fn leaf_expression(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let ty = self.known_type(expression, input, subqueries)?;
        let mut result = ExpressionType::leaf_owned(ty);
        let literal_collation = match expression {
            ScalarExpr::TypedLiteral { collation_oid, .. } => Some(*collation_oid),
            ScalarExpr::RetainedLiteral(literal) => Some(literal.collation_oid),
            _ => None,
        };
        if let Some(collation_oid) = literal_collation {
            crate::catalog::collations::expression::validate_literal_collation(
                result.ty.as_deref(),
                collation_oid,
            )?;
        }
        if let Some(oid) = input.input_collation(expression) {
            result.derivation = Derivation::implicit(i64::from(oid));
            result.initial_collation = oid;
        }
        self.expression_record_fields(&mut result, expression, input, subqueries)?;
        Ok(result)
    }
}
