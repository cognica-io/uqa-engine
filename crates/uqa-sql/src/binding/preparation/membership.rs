//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain membership analysis at the input-preparation boundary, before planning can duplicate or fold expressions. Each copied left-side subquery receives its own query-arena slot and therefore its own initialization cache.

use super::{ColumnType, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr};
use crate::{ast::BinaryOp, type_resolution::membership::MembershipShape};

pub(super) struct MembershipAnalysis {
    pub shape: MembershipShape,
    pub array_coercions: Vec<(usize, ColumnType)>,
    pub left_constants: Vec<Option<ScalarExpr>>,
}

impl Preparation<'_> {
    pub(super) fn in_list(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        let ScalarExpr::InList {
            expr,
            list,
            negated,
        } = expression
        else {
            unreachable!()
        };
        let needle = self.expression(expr, input, subqueries)?;
        let mut values = list
            .iter()
            .map(|item| self.expression(item, input, subqueries))
            .collect::<Result<Vec<_>, _>>()?;
        if needle.is_deferred() || values.iter().any(ExpressionType::is_deferred) {
            return Ok(());
        }
        let mut shape = MembershipShape::default();
        for (index, item) in list.iter().enumerate() {
            if !self.membership_item_references_row(item, input, subqueries)? {
                shape.array_items.push(index);
            }
        }
        let common = if shape.array_items.len() > 1 {
            let types = std::iter::once(needle.ty.as_ref())
                .chain(
                    shape
                        .array_items
                        .iter()
                        .map(|index| values[*index].ty.as_ref()),
                )
                .collect::<Vec<_>>();
            crate::type_resolution::select_common_input_type(&types)?
                .filter(|ty| !matches!(ty, ColumnType::Array(_) | ColumnType::Record))
        } else {
            None
        };
        if common.is_none() {
            shape.array_items.clear();
        }
        let mut analysis = MembershipAnalysis {
            shape,
            array_coercions: Vec::new(),
            left_constants: Vec::new(),
        };
        let op = if *negated {
            BinaryOp::NotEqual
        } else {
            BinaryOp::Equal
        };
        if let Some(common) = common {
            for index in &analysis.shape.array_items {
                let item = &mut values[*index];
                if item.ty.as_ref().is_some_and(|ty| *ty != common)
                    || matches!(list[*index], ScalarExpr::Literal(uqa_core::Value::Null))
                {
                    analysis.array_coercions.push((*index, common.clone()));
                }
                self.parameters.coerce_unknown(item, &common)?;
            }
            let mut left = needle.clone();
            self.binary(op, &mut left, &mut ExpressionType::resolved(Some(common)))?;
            analysis
                .left_constants
                .push(self.parameters.take_literal(expr));
        }
        for (index, item) in values.iter_mut().enumerate() {
            if analysis.shape.array_items.contains(&index) {
                continue;
            }
            let mut left = needle.clone();
            self.binary(op, &mut left, item)?;
            analysis
                .left_constants
                .push(self.parameters.take_literal(expr));
        }
        self.parameters.retain_membership(expression, analysis);
        Ok(())
    }

    fn membership_item_references_row(
        &self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<bool, SQLError> {
        if crate::semantics::references_current_row(expression, Some(input)) {
            return Ok(true);
        }
        let mut dependent = false;
        expression.try_visit(&mut |node| {
            let query = match node {
                ScalarExpr::ScalarSubquery(index)
                | ScalarExpr::Exists {
                    subquery: index, ..
                }
                | ScalarExpr::InSubquery {
                    subquery: index, ..
                } => Some(*index),
                _ => None,
            };
            if let Some(index) = query {
                let query = subqueries.get(index).ok_or_else(|| {
                    SQLError::Internal("membership subquery is outside its plan".into())
                })?;
                dependent |= crate::binding::correlation::query_depends_on_current_row(
                    crate::binding::correlation::CorrelationContext {
                        catalog: self.scope.catalog.as_ref(),
                        resolution: &self.scope.resolution,
                    },
                    query,
                    input,
                )?;
            }
            Ok::<_, SQLError>(!dependent)
        })?;
        Ok(dependent)
    }
}

impl MembershipAnalysis {
    pub(super) fn apply(
        self,
        expression: &mut ScalarExpr,
        arena: &mut Vec<QueryPlan>,
    ) -> Result<(), SQLError> {
        let ScalarExpr::InList {
            expr,
            mut list,
            negated,
        } = std::mem::replace(expression, ScalarExpr::Literal(uqa_core::Value::Null))
        else {
            return Err(SQLError::Internal(
                "retained membership no longer matches its syntax".into(),
            ));
        };
        for (index, target) in self.array_coercions {
            let item =
                std::mem::replace(&mut list[index], ScalarExpr::Literal(uqa_core::Value::Null));
            list[index] = ScalarExpr::Cast {
                implicit: true,
                expr: Box::new(item),
                ty: target.catalog_name(),
            };
        }
        *expression = crate::type_resolution::membership::rewrite(
            expr,
            list,
            negated,
            &self.shape,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )?
        .into_uncontrolled()
        .expect("ordinary input preparation");
        let comparisons = match expression {
            ScalarExpr::And(items) | ScalarExpr::Or(items) => items.as_mut_slice(),
            node => std::slice::from_mut(node),
        };
        let last = comparisons.len().saturating_sub(1);
        for (index, (comparison, constant)) in
            comparisons.iter_mut().zip(self.left_constants).enumerate()
        {
            let left = match comparison {
                ScalarExpr::Func { args, .. } => &mut args[0],
                ScalarExpr::Binary { lhs, .. } => lhs.as_mut(),
                _ => unreachable!("analyzed comparison"),
            };
            if let Some(constant) = constant {
                *left = constant;
            }
            if index < last {
                crate::plan::subqueries::copy_occurrences(left, arena)?;
            }
        }
        Ok(())
    }
}
