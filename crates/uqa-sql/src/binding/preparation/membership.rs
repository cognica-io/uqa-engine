//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain membership analysis at the input-preparation boundary, before planning can duplicate or fold expressions. Each copied left-side subquery receives its own query-arena slot and therefore its own initialization cache.

use super::ownership::{AnalysisType, AnalysisVec};
use super::{ColumnType, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr};
use crate::catalog::collations::derivation::Derivation;
use crate::{
    ast::BinaryOp,
    plan::{OwnedScalarExpr, ScalarQueryArena},
};
use uqa_core::memory::ProductionControl;

pub(super) struct MembershipAnalysis {
    pub array_items: AnalysisVec<usize>,
    pub array_coercions: AnalysisVec<(usize, AnalysisType)>,
    pub left_constants: AnalysisVec<Option<super::parameters::ConvertedInput>>,
}

impl Preparation<'_> {
    pub(super) fn in_list(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let ScalarExpr::InList {
            expr,
            list,
            negated,
        } = expression
        else {
            unreachable!()
        };
        let owner = self.scope.control().capture();
        let control = owner.control();
        let needle = self.expression(expr, input, subqueries)?;
        let mut values = AnalysisVec::new(&control);
        values.reserve(list.len(), &control)?;
        for item in list {
            values.push(self.expression(item, input, subqueries)?, &control)?;
        }
        if needle.is_deferred() || values.iter().any(ExpressionType::is_deferred) {
            let mut output = ExpressionType::independent(Some(ColumnType::Boolean), &values);
            output.derivation = needle.derivation.error_only().combine(output.derivation);
            return Ok(output);
        }
        let mut array_items = AnalysisVec::new(&control);
        for (index, item) in list.iter().enumerate() {
            if !self.membership_item_references_row(item, input, subqueries)? {
                array_items.push(index, &control)?;
            }
        }
        let common = membership_array_type(&needle, &values, &array_items, &control)?;
        if common.is_none() {
            array_items.clear();
        }
        let mut analysis = MembershipAnalysis {
            array_items,
            array_coercions: AnalysisVec::new(&control),
            left_constants: AnalysisVec::new(&control),
        };
        let op = if *negated {
            BinaryOp::NotEqual
        } else {
            BinaryOp::Equal
        };
        let mut selected = Derivation::None;
        if let Some(common) = common {
            for index in &analysis.array_items {
                let item = &mut values[*index];
                if item.ty.as_deref().is_some_and(|ty| *ty != *common)
                    || matches!(list[*index], ScalarExpr::Literal(uqa_core::Value::Null))
                {
                    analysis
                        .array_coercions
                        .push((*index, common.clone()), &control)?;
                }
                self.parameters.coerce_unknown(item, &common)?;
            }
            let mut left = needle.copy_with_control(&self.scope.control())?;
            let array = analysis
                .array_items
                .iter()
                .fold(Derivation::None, |state, index| {
                    state.combine(values[*index].derivation)
                })
                .result(crate::catalog::type_metadata::pg_type_collation_oid(
                    &common,
                ));
            self.binary(
                op,
                &mut left,
                &mut ExpressionType::resolved_owned(Some(common)),
            )?;
            selected = selected.combine(left.derivation.combine(array).error_only());
            analysis
                .left_constants
                .push(self.parameters.take_literal(expr)?, &control)?;
        }
        for (index, item) in values.iter_mut().enumerate() {
            if analysis.array_items.contains(&index) {
                continue;
            }
            let mut left = needle.copy_with_control(&self.scope.control())?;
            self.binary(op, &mut left, item)?;
            selected = selected.combine(ExpressionType::comparison(&left, item));
            self.validate_membership_operands(expr, &list[index], input)?;
            analysis
                .left_constants
                .push(self.parameters.take_literal(expr)?, &control)?;
        }
        self.parameters.retain_membership(expression, analysis)?;
        let mut output = ExpressionType::resolved(Some(ColumnType::Boolean));
        output.derivation = selected;
        Ok(output)
    }

    /// `PostgreSQL` checks each boolean comparison after operator resolution; its array comparison permits set-valued inputs.
    fn validate_membership_operands(
        &self,
        left: &ScalarExpr,
        right: &ScalarExpr,
        input: &RowSchema,
    ) -> Result<(), SQLError> {
        if !crate::semantics::sets::placement_scope::placing_set_calls() {
            return Ok(());
        }
        let params = self.parameters.values()?;
        for operand in [left, right] {
            if crate::semantics::sets::validation::expression_may_return_set_with_control(
                self.routines,
                self.routines,
                operand,
                input,
                &params,
                &self.scope.control(),
            )? {
                return Err(crate::semantics::sets::placement::set_argument_error("IN"));
            }
        }
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
                        routines: self.routines,
                        catalog: self.scope.catalog.as_ref(),
                        resolution: &self.scope.resolution,
                        scoped_relations: None,
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
        let control = ProductionControl::uncontrolled();
        let original = std::mem::replace(expression, ScalarExpr::Literal(uqa_core::Value::Null));
        *expression = self
            .apply_with(
                OwnedScalarExpr::uncontrolled(original),
                &control,
                &mut |left| {
                    let mut left = left
                        .finish(&control)?
                        .into_uncontrolled()
                        .expect("ordinary membership left operand");
                    crate::plan::subqueries::copy_occurrences(&mut left, arena)?;
                    Ok(OwnedScalarExpr::uncontrolled(left))
                },
            )?
            .finish(&control)?
            .into_uncontrolled()
            .expect("ordinary membership input");
        Ok(())
    }

    pub(super) fn apply_owned(
        self,
        expression: OwnedScalarExpr,
        arena: &mut ScalarQueryArena,
        control: &ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        self.apply_with(expression, control, &mut |left| {
            arena.copy_subquery_occurrences(left, control)
        })
    }

    fn apply_with(
        self,
        mut expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        copy_queries: &mut impl FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        control.check()?;
        if !matches!(&*expression, ScalarExpr::InList { .. }) {
            return Err(SQLError::Internal(
                "retained membership no longer matches its syntax".into(),
            ));
        }
        for (index, target) in self.array_coercions {
            expression = expression.map_child(index + 1, control, |item| {
                OwnedScalarExpr::cast(
                    item,
                    target.catalog_name_with_control(control)?,
                    true,
                    control,
                )
            })?;
        }
        expression = crate::type_resolution::membership::rewrite_owned(
            expression,
            &self.array_items,
            control,
        )?;
        if self.left_constants.is_empty() {
            return Ok(expression);
        }
        let multiple = matches!(&*expression, ScalarExpr::And(_) | ScalarExpr::Or(_));
        let count = if multiple {
            expression.child_count()
        } else {
            1
        };
        if count != self.left_constants.len() {
            return Err(SQLError::Internal(
                "analyzed membership comparison count changed".into(),
            ));
        }
        let last = count.saturating_sub(1);
        for (index, constant) in self.left_constants.into_iter().enumerate() {
            let apply = |comparison: OwnedScalarExpr| {
                comparison.map_child(0, control, |left| {
                    let left = constant.map_or(left, |constant| constant.expression);
                    if index < last {
                        copy_queries(left)
                    } else {
                        Ok(left)
                    }
                })
            };
            expression = if multiple {
                expression.map_child(index, control, apply)?
            } else {
                apply(expression)?
            };
        }
        Ok(expression)
    }
}

fn membership_array_type(
    needle: &ExpressionType,
    values: &[ExpressionType],
    array_items: &[usize],
    control: &ProductionControl<'_>,
) -> Result<Option<AnalysisType>, SQLError> {
    if array_items.len() <= 1 {
        return Ok(None);
    }
    let mut types = AnalysisVec::new(control);
    types.reserve(1 + array_items.len(), control)?;
    types.push(needle.ty.as_deref(), control)?;
    for index in array_items {
        types.push(values[*index].ty.as_deref(), control)?;
    }
    crate::type_resolution::select_common_input_type_with_control(&types, control)?
        .filter(|ty| !matches!(**ty, ColumnType::Array(_) | ColumnType::Record))
        .map(|ty| AnalysisType::from_produced(ty, control))
        .transpose()
}
