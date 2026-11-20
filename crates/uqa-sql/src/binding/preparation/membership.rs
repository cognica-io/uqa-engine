//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain membership analysis at the input-preparation boundary, before planning can duplicate or fold expressions. Each copied left-side subquery receives its own query-arena slot and therefore its own initialization cache.

use crate::binding::preparation::{LiteralInput, SchemaContext};

use super::ownership::{AnalysisType, AnalysisVec};
use super::{ColumnType, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr};
use crate::catalog::{
    collations::derivation::Derivation,
    work::{ReferenceWork, Work},
};
use crate::{
    ast::BinaryOp,
    plan::{OwnedScalarExpr, ScalarQueryArena},
};
use uqa_core::memory::ProductionControl;

mod application;

pub(super) struct MembershipAnalysis {
    pub array_items: AnalysisVec<usize>,
    pub array_coercions: AnalysisVec<(usize, AnalysisType)>,
    pub left_constants: AnalysisVec<Option<super::parameters::ConvertedInput>>,
}

impl<I: LiteralInput, C: SchemaContext> Preparation<'_, I, C> {
    /// Construct the ordinary membership future on the selected call frame.
    pub(super) fn in_list_inline<W: Work + Send>(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
        work: &mut W,
    ) -> Result<ExpressionType, SQLError> {
        crate::catalog::work::inline_with(|| self.in_list(expression, input, subqueries, work))
    }

    pub(super) async fn in_list<W: Work + Send>(
        &mut self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
        work: &mut W,
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
        let needle = self
            .membership_operand(expr, input, subqueries, &control, work)
            .await?;
        let mut deferred = needle.is_deferred();
        let mut output = ExpressionType::resolved(Some(ColumnType::Boolean));
        let mut values = AnalysisVec::new(&control);
        values.reserve(list.len(), &control)?;
        for item in list {
            let value = self
                .membership_operand(item, input, subqueries, &control, work)
                .await?;
            deferred |= value.is_deferred();
            output.derivation = output.derivation.combine(value.derivation.error_only());
            values.push(value, &control)?;
        }
        if deferred {
            output.derivation = needle.derivation.error_only().combine(output.derivation);
            return Ok(output);
        }
        let mut array_items = self
            .membership_array_candidates(list, input, subqueries, &control, work)
            .await?;
        let common = membership_array_type(&needle, &values, &array_items, &control, work).await?;
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
            let array = analysis
                .coerce_array_items(
                    list,
                    &mut values,
                    &common,
                    &mut self.parameters,
                    &control,
                    work,
                )
                .await?;
            let mut left = needle.copy_with_control(&control)?;
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
        let mut array_items = analysis.array_items.iter().copied().peekable();
        for (index, item) in values.iter_mut().enumerate() {
            ReferenceWork {
                control: &control,
                work,
            }
            .checkpoint()
            .await?;
            if array_items.next_if_eq(&index).is_some() {
                continue;
            }
            let mut left = needle.copy_with_control(&control)?;
            self.binary(op, &mut left, item)?;
            selected = selected.combine(ExpressionType::comparison(&left, item));
            self.validate_membership_operands(expr, &list[index], input)?;
            analysis
                .left_constants
                .push(self.parameters.take_literal(expr)?, &control)?;
        }
        self.parameters.retain_membership(expression, analysis)?;
        output.derivation = selected;
        Ok(output)
    }

    async fn membership_operand<W: Work + Send>(
        &mut self,
        operand: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<ExpressionType, SQLError> {
        ReferenceWork { control, work }.checkpoint().await?;
        if W::MAY_SUSPEND && !matches!(operand, ScalarExpr::Literal(_) | ScalarExpr::Param(_)) {
            crate::construction::retain_with(*control, || {
                self.expression_with_work(operand, input, subqueries, work)
            })?
            .await
        } else {
            self.expression(operand, input, subqueries)
        }
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

    async fn membership_array_candidates(
        &self,
        list: &[ScalarExpr],
        input: &RowSchema,
        subqueries: &[QueryPlan],
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<AnalysisVec<usize>, SQLError> {
        let mut candidates = AnalysisVec::new(control);
        for (index, item) in list.iter().enumerate() {
            if !self
                .membership_item_references_row(item, input, subqueries, control, work)
                .await?
            {
                candidates.push(index, control)?;
            }
        }
        Ok(candidates)
    }

    async fn membership_item_references_row<W: Work + Send>(
        &self,
        expression: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<bool, SQLError> {
        use crate::ir::{
            children::Scope,
            observation::{visit_in, Decision},
        };
        if visit_in(expression, control, work, Scope::Complete, &mut |node| {
            Ok(
                if crate::semantics::node_references_current_row(node, Some(input)) {
                    Decision::Stop
                } else {
                    Decision::Descend
                },
            )
        })
        .await?
        {
            return Ok(true);
        }
        let mut dependent = false;
        visit_in(expression, control, work, Scope::Complete, &mut |node| {
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
            Ok(if dependent {
                Decision::Prune
            } else {
                Decision::Descend
            })
        })
        .await?;
        Ok(dependent)
    }
}

impl MembershipAnalysis {
    async fn coerce_array_items<I: LiteralInput>(
        &mut self,
        list: &[ScalarExpr],
        values: &mut [ExpressionType],
        common: &AnalysisType,
        parameters: &mut super::ParameterTypes<'_, I>,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<Derivation, SQLError> {
        let mut derivation = Derivation::None;
        for &index in &self.array_items {
            ReferenceWork { control, work }.checkpoint().await?;
            let item = &mut values[index];
            if item.ty.as_deref().is_some_and(|ty| ty != &**common)
                || matches!(list[index], ScalarExpr::Literal(uqa_core::Value::Null))
            {
                self.array_coercions
                    .push((index, common.clone()), control)?;
            }
            parameters.coerce_unknown(item, common)?;
            derivation = derivation.combine(item.derivation);
        }
        Ok(derivation.result(crate::catalog::type_metadata::pg_type_collation_oid(common)))
    }

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
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        copy_queries: &mut impl FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        crate::catalog::work::inline(self.apply_from(
            expression,
            control,
            &mut (),
            &mut application::Immediate(copy_queries),
        ))
    }
}

async fn membership_array_type<W: Work + Send>(
    needle: &ExpressionType,
    values: &[ExpressionType],
    array_items: &[usize],
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<AnalysisType>, SQLError> {
    if array_items.len() <= 1 {
        return Ok(None);
    }
    let types = std::iter::once(needle.ty.as_deref())
        .chain(array_items.iter().map(|index| values[*index].ty.as_deref()));
    crate::type_resolution::select_common_input_type_with_work(types, control, work)
        .await?
        .filter(|ty| !matches!(**ty, ColumnType::Array(_) | ColumnType::Record))
        .map(|ty| AnalysisType::from_produced(ty, control))
        .transpose()
}
