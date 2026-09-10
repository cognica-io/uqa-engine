//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! OLD/NEW reference analysis for rewrite-rule expressions and stored plans.

use std::collections::BTreeSet;

use uqa_execution::ScalarExpr;
use uqa_planner::ExpressionPlan;
use uqa_sql::ast::{Expr, SelectStmt, Statement};
use uqa_sql::plpgsql::{ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

use super::{bind_rule_action, bind_rule_expr_scoped, bind_rule_select_scoped};
use crate::engine_events::RuleConditionBinding;

#[derive(Default)]
struct RuleRowReferenceDetector {
    qualifier: Option<String>,
    whole_row: bool,
}

#[derive(Default)]
struct RuleRowColumnCollector {
    columns: BTreeSet<String>,
    qualifier: Option<&'static str>,
    whole_row: bool,
}

impl RuleRowColumnCollector {
    fn accepts(&self, qualifier: &str) -> bool {
        self.qualifier.map_or_else(
            || qualifier.eq_ignore_ascii_case("old") || qualifier.eq_ignore_ascii_case("new"),
            |selected| selected.eq_ignore_ascii_case(qualifier),
        )
    }
}

impl VariableResolver for RuleRowColumnCollector {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        self.whole_row |= self.accepts(name);
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if self.accepts(qualifier) {
            self.columns.insert(column.to_string());
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        self.whole_row |= self.accepts(qualifier);
        Ok(None)
    }
}

impl VariableResolver for RuleRowReferenceDetector {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        if name.eq_ignore_ascii_case("old") || name.eq_ignore_ascii_case("new") {
            self.observe(name, true);
        }
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        self.observe(qualifier, false);
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        self.observe(qualifier, true);
        Ok(None)
    }
}

impl RuleRowReferenceDetector {
    fn observe(&mut self, qualifier: &str, whole_row: bool) {
        if qualifier.eq_ignore_ascii_case("old") || qualifier.eq_ignore_ascii_case("new") {
            if self.qualifier.is_none() {
                self.qualifier = Some(qualifier.to_ascii_lowercase());
            }
            self.whole_row |= whole_row;
        }
    }
}

pub(crate) fn rule_action_has_set_operation(action: &Statement) -> bool {
    match action {
        Statement::Select(select) => select.set_op.is_some(),
        Statement::Insert(insert) => insert
            .select_source
            .as_ref()
            .is_some_and(|select| select.set_op.is_some()),
        _ => false,
    }
}

pub(crate) fn first_rule_row_reference_in_expr(
    expr: &Expr,
    shadowed: &BTreeSet<String>,
) -> Option<String> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_expr_scoped(expr, &mut detector, shadowed);
    detector.qualifier
}

pub(crate) fn first_rule_row_reference_in_select(
    engine: &crate::Engine,
    select: &SelectStmt,
) -> Option<String> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_select_scoped(engine, select, &mut detector);
    detector.qualifier
}

pub(crate) fn rule_expr_references_row(expr: &Expr) -> bool {
    first_rule_row_reference_in_expr(expr, &BTreeSet::new()).is_some()
}

pub(crate) fn rule_expr_references_whole_row(expr: &Expr) -> bool {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_expr_scoped(expr, &mut detector, &BTreeSet::new());
    detector.whole_row
}

pub(crate) fn first_rule_row_reference_in_statement(
    engine: &crate::Engine,
    statement: &Statement,
) -> Result<Option<String>, SQLError> {
    let target = match statement {
        Statement::Insert(plan) => Some(plan.table.as_str()),
        Statement::Update(plan) => Some(plan.table.as_str()),
        Statement::Delete(plan) => Some(plan.table.as_str()),
        Statement::Merge(plan) => Some(plan.target.as_str()),
        _ => None,
    };
    let columns = target
        .map(|table| crate::sql::query_source_column_names(engine, table, false))
        .transpose()?
        .flatten()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_action(engine, statement, &columns, &mut detector)?;
    Ok(detector.qualifier)
}

pub(crate) fn rule_statement_references_row(
    engine: &crate::Engine,
    statement: &Statement,
    action_columns: &BTreeSet<String>,
) -> Result<bool, SQLError> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_action(engine, statement, action_columns, &mut detector)?;
    Ok(detector.qualifier.is_some())
}

pub(crate) fn rule_statement_references_whole_row(
    engine: &crate::Engine,
    statement: &Statement,
    action_columns: &BTreeSet<String>,
) -> Result<bool, SQLError> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_action(engine, statement, action_columns, &mut detector)?;
    Ok(detector.whole_row)
}

pub(crate) fn rule_expr_row_columns(expr: &Expr) -> BTreeSet<String> {
    let mut collector = RuleRowColumnCollector::default();
    let _ = bind_rule_expr_scoped(expr, &mut collector, &BTreeSet::new());
    collector.columns
}

fn collect_condition_plan_row_column(
    expression: &ScalarExpr,
    binding: &RuleConditionBinding,
    columns: &mut BTreeSet<String>,
) {
    if let ScalarExpr::InternalColumn(column) = expression {
        if let Some(name) = binding.column_name(*column) {
            columns.insert(name.to_string());
        }
    }
}

fn condition_plan_expression_references_whole_row(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Column(qualifier) | ScalarExpr::QualifiedStar(qualifier)
            if qualifier.eq_ignore_ascii_case("old")
                || qualifier.eq_ignore_ascii_case("new")
    )
}

pub(crate) fn rule_condition_plan_references_whole_row(plan: &ExpressionPlan) -> bool {
    let mut referenced = false;
    plan.scalar.visit(&mut |expression| {
        referenced |= condition_plan_expression_references_whole_row(expression);
    });
    for subquery in &plan.subqueries {
        let mut subquery = subquery.clone();
        subquery.rewrite_scalar_expressions(&mut |expression| {
            referenced |= condition_plan_expression_references_whole_row(expression);
        });
    }
    referenced
}

pub(crate) fn rule_condition_plan_row_columns(
    plan: &ExpressionPlan,
    binding: &RuleConditionBinding,
) -> BTreeSet<String> {
    let mut columns = BTreeSet::new();
    plan.scalar.visit(&mut |expression| {
        collect_condition_plan_row_column(expression, binding, &mut columns);
    });
    for subquery in &plan.subqueries {
        let mut subquery = subquery.clone();
        subquery.rewrite_scalar_expressions(&mut |expression| {
            collect_condition_plan_row_column(expression, binding, &mut columns);
        });
    }
    columns
}

pub(crate) fn rule_statement_row_columns(
    engine: &crate::Engine,
    statement: &Statement,
    action_columns: &BTreeSet<String>,
) -> Result<BTreeSet<String>, SQLError> {
    let mut collector = RuleRowColumnCollector::default();
    let _ = bind_rule_action(engine, statement, action_columns, &mut collector)?;
    Ok(collector.columns)
}

/// Input assignments contribute only NEW fields; OLD references require source rows but never the corresponding new assignment expression.
pub(crate) fn rule_new_row_columns(
    engine: &crate::Engine,
    rule: &crate::engine_events::StoredRule,
) -> Result<Option<BTreeSet<String>>, SQLError> {
    let mut collector = RuleRowColumnCollector {
        qualifier: Some("new"),
        ..RuleRowColumnCollector::default()
    };
    if let Some(condition) = &rule.definition.condition {
        bind_rule_expr_scoped(condition, &mut collector, &BTreeSet::new())?;
    }
    for action in &rule.definition.actions {
        let columns = engine.rule_action_target_columns(action)?;
        bind_rule_action(engine, action, &columns, &mut collector)?;
    }
    Ok((!collector.whole_row).then_some(collector.columns))
}
