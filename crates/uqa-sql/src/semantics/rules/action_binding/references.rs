//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! OLD/NEW reference analysis for rewrite-rule expressions and stored plans.

use std::collections::BTreeSet;

use crate::ast::{Expr, SelectStmt, Statement};
use crate::plan::ExpressionPlan;
use crate::plpgsql::{ResolvedVariable, VariableResolver};
use crate::SQLError;
use crate::ScalarExpr;

use super::{
    inspection,
    names::{self, Names},
};
use crate::catalog::events::RuleConditionBinding;
use uqa_core::memory::{OwnedProductionControl, Produced, ProductionControl, ProductionVec};

#[derive(Default)]
struct RuleRowReferenceDetector {
    qualifier: Option<String>,
    whole_row: bool,
}

struct RuleRowColumnCollector {
    columns: Names,
    control: OwnedProductionControl,
    qualifier: Option<&'static str>,
    whole_row: bool,
}

impl Default for RuleRowColumnCollector {
    fn default() -> Self {
        Self::new(&ProductionControl::uncontrolled())
    }
}
impl RuleRowColumnCollector {
    fn new(control: &ProductionControl<'_>) -> Self {
        Self {
            columns: Names::new(),
            control: control.capture(),
            qualifier: None,
            whole_row: false,
        }
    }
    fn into_uncontrolled(self) -> BTreeSet<String> {
        self.columns
            .into_iter()
            .map(|name| name.into_uncontrolled().expect("ordinary rule reference"))
            .collect()
    }
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
            names::insert_name(&mut self.columns, column, false, &self.control.control())?;
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

pub fn rule_action_has_set_operation(action: &Statement) -> bool {
    match action {
        Statement::Select(select) => select.set_op.is_some(),
        Statement::Insert(insert) => insert
            .select_source
            .as_ref()
            .is_some_and(|select| select.set_op.is_some()),
        _ => false,
    }
}

pub fn first_rule_row_reference_in_expr(
    expr: &Expr,
    shadowed: &BTreeSet<String>,
) -> Option<String> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = inspection::expression(expr, &mut detector, shadowed);
    detector.qualifier
}

pub fn first_rule_row_reference_in_select(
    catalog: &dyn super::RuleSourceCatalog,
    select: &SelectStmt,
) -> Option<String> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = inspection::select(catalog, select, &mut detector);
    detector.qualifier
}

pub fn rule_expr_references_row(expr: &Expr) -> bool {
    first_rule_row_reference_in_expr(expr, &BTreeSet::new()).is_some()
}

pub fn rule_expr_references_whole_row(expr: &Expr) -> bool {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = inspection::expression(expr, &mut detector, &BTreeSet::new());
    detector.whole_row
}

pub fn first_rule_row_reference_in_statement(
    catalog: &dyn super::RuleSourceCatalog,
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
        .map(|table| catalog.query_source_columns(table, false))
        .transpose()?
        .flatten()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut detector = RuleRowReferenceDetector::default();
    inspection::action(catalog, statement, &columns, &mut detector)?;
    Ok(detector.qualifier)
}

pub fn rule_statement_references_row(
    catalog: &dyn super::RuleSourceCatalog,
    statement: &Statement,
    action_columns: &BTreeSet<String>,
) -> Result<bool, SQLError> {
    let mut detector = RuleRowReferenceDetector::default();
    inspection::action(catalog, statement, action_columns, &mut detector)?;
    Ok(detector.qualifier.is_some())
}

pub fn rule_statement_references_whole_row(
    catalog: &dyn super::RuleSourceCatalog,
    statement: &Statement,
    action_columns: &BTreeSet<String>,
) -> Result<bool, SQLError> {
    let mut detector = RuleRowReferenceDetector::default();
    inspection::action(catalog, statement, action_columns, &mut detector)?;
    Ok(detector.whole_row)
}

pub fn rule_expr_row_columns(expr: &Expr) -> BTreeSet<String> {
    let mut collector = RuleRowColumnCollector::default();
    let _ = inspection::expression(expr, &mut collector, &BTreeSet::new());
    collector.into_uncontrolled()
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

pub fn rule_condition_plan_references_whole_row(plan: &ExpressionPlan) -> bool {
    let mut referenced = false;
    plan.scalar.visit(&mut |expression| {
        referenced |= condition_plan_expression_references_whole_row(expression);
    });
    for subquery in &plan.subqueries {
        subquery.visit_scalar_expressions(&mut |root| {
            root.visit(&mut |expression| {
                referenced |= condition_plan_expression_references_whole_row(expression);
            });
        });
    }
    referenced
}

pub fn rule_condition_plan_row_columns(
    plan: &ExpressionPlan,
    binding: &RuleConditionBinding,
) -> BTreeSet<String> {
    let mut columns = BTreeSet::new();
    plan.scalar.visit(&mut |expression| {
        collect_condition_plan_row_column(expression, binding, &mut columns);
    });
    for subquery in &plan.subqueries {
        subquery.visit_scalar_expressions(&mut |root| {
            root.visit(&mut |expression| {
                collect_condition_plan_row_column(expression, binding, &mut columns);
            });
        });
    }
    columns
}

pub fn rule_statement_row_columns(
    catalog: &dyn super::RuleSourceCatalog,
    statement: &Statement,
    action_columns: &BTreeSet<String>,
) -> Result<BTreeSet<String>, SQLError> {
    let mut collector = RuleRowColumnCollector::default();
    inspection::action(catalog, statement, action_columns, &mut collector)?;
    Ok(collector.into_uncontrolled())
}

/// Input assignments contribute only NEW fields; OLD references require source rows but never the corresponding new assignment expression.
pub fn rule_new_row_columns(
    catalog: &dyn super::RuleSourceCatalog,
    rule: &crate::catalog::events::StoredRule,
) -> Result<Option<BTreeSet<String>>, SQLError> {
    Ok(
        rule_new_row_columns_with_control(catalog, rule, &ProductionControl::uncontrolled())?.map(
            |columns| {
                columns
                    .into_uncontrolled()
                    .expect("ordinary rule reference")
                    .into_iter()
                    .collect()
            },
        ),
    )
}

pub fn rule_new_row_columns_with_control(
    catalog: &dyn super::RuleSourceCatalog,
    rule: &crate::catalog::events::StoredRule,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<String>>>, SQLError> {
    control.check()?;
    let mut collector = RuleRowColumnCollector {
        qualifier: Some("new"),
        ..RuleRowColumnCollector::new(control)
    };
    if let Some(condition) = &rule.definition.condition {
        inspection::expression_with_control(condition, &mut collector, &BTreeSet::new(), control)?;
    }
    for action in &rule.definition.actions {
        control.check()?;
        let target = match action {
            Statement::Insert(statement) => Some(statement.table.as_str()),
            Statement::Update(statement) => Some(statement.table.as_str()),
            Statement::Delete(statement) => Some(statement.table.as_str()),
            _ => None,
        };
        let mut columns = Names::new();
        if let Some(target) = target {
            let selected = catalog.rule_relation_columns_with_control(target, control)?;
            for name in selected.iter() {
                names::insert_name(&mut columns, name, false, control)?;
            }
        }
        inspection::action_with_control(catalog, action, &columns, &mut collector, control)?;
    }
    if collector.whole_row {
        return Ok(None);
    }
    let mut output = ProductionVec::new(*control);
    for column in collector.columns {
        output.push_produced(column)?;
    }
    Ok(Some(output.finish()?))
}
