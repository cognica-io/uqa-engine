//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Event-row discovery lends stored rule trees and admits selected OLD/NEW names.

use super::{
    condition_plan_expression_references_whole_row, inspection, names, RuleRowColumnCollector,
    RuleRowReferenceDetector,
};
use crate::{
    ast::RuleEvent, catalog::events::RuleConditionBinding, plan::ExpressionPlan,
    semantics::rules::analysis::RuleAnalysisContext, SQLError, ScalarExpr,
};
use std::collections::BTreeSet;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub fn relation_rule_row_columns_with_control(
    context: RuleAnalysisContext<'_>,
    table: &str,
    event: RuleEvent,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<String>>>, SQLError> {
    control.check()?;
    let mut collector = RuleRowColumnCollector::new(control);
    let mut references_row = false;
    let mut whole_row = false;
    for rule in context
        .rules
        .capture_rules_with_control(table, event, control)?
        .iter()
    {
        control.check()?;
        let rule = rule?;
        if let Some((plan, binding)) = rule.bound_condition_plan() {
            whole_row |= bound_condition(plan, binding, &mut collector, control)?;
        } else if let Some(condition) = rule.definition.condition.as_ref() {
            let mut detector = RuleRowReferenceDetector::default();
            condition_result(inspection::expression_with_control(
                condition,
                &mut detector,
                &BTreeSet::new(),
                control,
            ))?;
            references_row |= detector.qualifier.is_some();
            whole_row |= detector.whole_row;
            condition_result(inspection::expression_with_control(
                condition,
                &mut collector,
                &BTreeSet::new(),
                control,
            ))?;
        }
        for action in &rule.definition.actions {
            let columns = super::super::rule_action_target_columns_with_control(
                context.sources,
                action,
                control,
            )?;
            let mut detector = RuleRowReferenceDetector::default();
            inspection::action_with_control(
                context.sources,
                action,
                &*columns,
                &mut detector,
                control,
            )?;
            references_row |= detector.qualifier.is_some();
            // Keep the original reference/whole-row/column catalog observation order.
            let mut detector = RuleRowReferenceDetector::default();
            inspection::action_with_control(
                context.sources,
                action,
                &*columns,
                &mut detector,
                control,
            )?;
            whole_row |= detector.whole_row;
            inspection::action_with_control(
                context.sources,
                action,
                &*columns,
                &mut collector,
                control,
            )?;
        }
    }
    if whole_row || references_row && collector.columns.is_empty() {
        return Ok(None);
    }
    let mut output = ProductionVec::new(*control);
    for name in collector.columns {
        output.push_produced(name)?;
    }
    Ok(Some(output.finish()?))
}

fn condition_result(result: Result<(), SQLError>) -> Result<(), SQLError> {
    match result {
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        _ => Ok(()),
    }
}

fn bound_condition(
    plan: &ExpressionPlan,
    binding: &RuleConditionBinding,
    collector: &mut RuleRowColumnCollector,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let mut whole_row = false;
    let mut observe = |root: &ScalarExpr| {
        root.try_visit(&mut |expression| {
            control.check()?;
            whole_row |= condition_plan_expression_references_whole_row(expression);
            if let ScalarExpr::InternalColumn(column) = expression {
                if let Some(name) = binding.column_name(*column) {
                    names::insert_name(&mut collector.columns, name, false, control)?;
                }
            }
            Ok::<_, SQLError>(true)
        })
    };
    observe(&plan.scalar)?;
    for query in &plan.subqueries {
        query.try_visit_scalar_expressions(&mut observe)?;
    }
    Ok(whole_row)
}
