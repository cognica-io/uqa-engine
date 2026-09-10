//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View mutation target identity, declared columns, and rewrite-rule input analysis.
use super::{
    rules::analysis::RuleAnalysisContext,
    view_rewrite::context::{stored_view_schema, ViewRewriteContext},
};
use crate::{
    catalog::stored_view::StoredView,
    plan::{DeletePlan, MergePlan, MergeWhenPlan, UpdatePlan},
    ColumnType, SQLError, ScalarExpr,
};
use std::collections::BTreeSet;
use uqa_core::Value;

pub struct ViewMutationTarget {
    pub canonical_name: String,
    pub definition: StoredView,
    pub columns: Vec<String>,
    pub types: Vec<Option<ColumnType>>,
}

pub fn resolve_view_target(
    context: ViewRewriteContext<'_>,
    name: &str,
) -> Result<ViewMutationTarget, SQLError> {
    let canonical_name = context
        .catalog
        .try_resolve_view_name(name)
        .map_err(|error| SQLError::Internal(format!("resolve DML view `{name}`: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
    let definition = context
        .authorization
        .view_definition(&canonical_name)?
        .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
    if definition.kind != crate::catalog::view::StoredViewKind::View {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("relation \"{canonical_name}\" is not a view"),
        });
    }
    let schema = stored_view_schema(context, &definition.rewrite_definition())?;
    let columns = schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| schema.public_name(position).unwrap_or(column).to_string())
        .collect::<Vec<_>>();
    let types = (0..columns.len())
        .map(|position| schema.column_type(position).cloned())
        .collect();
    Ok(ViewMutationTarget {
        canonical_name,
        definition,
        columns,
        types,
    })
}

fn collect_view_expression_columns(
    expression: &ScalarExpr,
    columns: &mut BTreeSet<String>,
) -> bool {
    expression.collect_columns(columns)
}

pub fn required_view_update_columns(
    analysis: RuleAnalysisContext<'_>,
    target: &ViewMutationTarget,
    stmt: &UpdatePlan,
) -> Result<Option<BTreeSet<String>>, SQLError> {
    let Some(mut columns) = super::rules::analysis::relation_rule_row_columns(
        analysis,
        &target.canonical_name,
        crate::ast::RuleEvent::Update,
    )?
    else {
        return Ok(None);
    };
    columns.extend(
        stmt.assignments
            .iter()
            .map(|assignment| assignment.column.clone()),
    );
    for assignment in &stmt.assignments {
        if !collect_view_expression_columns(&assignment.value, &mut columns) {
            return Ok(None);
        }
    }
    if let Some(predicate) = stmt.predicate.as_ref() {
        if !collect_view_expression_columns(predicate, &mut columns) {
            return Ok(None);
        }
    }
    Ok(Some(columns))
}

pub fn required_view_delete_columns(
    analysis: RuleAnalysisContext<'_>,
    target: &ViewMutationTarget,
    stmt: &DeletePlan,
) -> Result<Option<BTreeSet<String>>, SQLError> {
    let Some(mut columns) = super::rules::analysis::relation_rule_row_columns(
        analysis,
        &target.canonical_name,
        crate::ast::RuleEvent::Delete,
    )?
    else {
        return Ok(None);
    };
    if let Some(predicate) = stmt.predicate.as_ref() {
        if !collect_view_expression_columns(predicate, &mut columns) {
            return Ok(None);
        }
    }
    Ok(Some(columns))
}

pub fn target_columns(
    target: &ViewMutationTarget,
    explicit: &[String],
    operation: &str,
) -> Result<Vec<String>, SQLError> {
    let columns = if explicit.is_empty() {
        target.columns.clone()
    } else {
        explicit.to_vec()
    };
    let mut seen = BTreeSet::new();
    for column in &columns {
        if !seen.insert(column) {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column \"{column}\" specified more than once"),
            });
        }
        if !target.columns.contains(column) {
            return Err(SQLError::UnknownColumn(format!(
                "{}.{column}",
                target.canonical_name
            )));
        }
    }
    if columns.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{operation} against a zero-column view is not supported"
        )));
    }
    Ok(columns)
}

pub fn view_qualification_references_target(
    target: &ViewMutationTarget,
    target_qualifier: &str,
    predicate: Option<&ScalarExpr>,
) -> bool {
    let Some(predicate) = predicate else {
        return false;
    };
    if super::expr_contains_subquery(predicate) {
        return true;
    }
    if super::expr_qualifiers(predicate).iter().any(|qualifier| {
        qualifier.eq_ignore_ascii_case(target_qualifier)
            || qualifier.eq_ignore_ascii_case(&target.canonical_name)
    }) {
        return true;
    }
    if !super::expr_has_unqualified_column(predicate) {
        return false;
    }
    let mut columns = BTreeSet::new();
    !predicate.collect_columns(&mut columns)
        || columns.iter().any(|column| target.columns.contains(column))
}

pub fn coerce_view_value(
    assignment: &dyn crate::assignment::AssignmentContext,
    target: &ViewMutationTarget,
    position: usize,
    value: Value,
) -> Result<Value, SQLError> {
    match target.types[position].as_ref() {
        Some(ty) => crate::assignment::conversion::convert_value_to_column_type_with_context(
            assignment, value, ty,
        ),
        None => Ok(value),
    }
}

pub fn validate_view_merge_targets(
    target: &ViewMutationTarget,
    plan: &MergePlan,
) -> Result<(), SQLError> {
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                let columns = assignments
                    .iter()
                    .map(|assignment| assignment.column.clone())
                    .collect::<Vec<_>>();
                let _ = target_columns(target, &columns, "UPDATE")?;
            }
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                let implicit = columns.is_empty();
                let columns = target_columns(target, columns, "INSERT")?;
                if values.len() > columns.len() || (!implicit && values.len() != columns.len()) {
                    return Err(SQLError::TypeMismatch(format!(
                        "MERGE INSERT row width {} != column count {}",
                        values.len(),
                        columns.len()
                    )));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn validate_view_merge_contract(
    rewrite: ViewRewriteContext<'_>,
    target: &ViewMutationTarget,
    plan: &MergePlan,
    params: &[crate::SQLParam],
    bindings: &crate::binding::context::BindingContext<'_>,
) -> Result<(), SQLError> {
    validate_view_merge_targets(target, plan)?;
    let source = crate::binding::analyze_source_plan_schema(
        rewrite.catalog,
        &plan.source,
        params,
        bindings,
        None,
    )?;
    super::view_rewrite::validate_public_merge_targets(rewrite, plan)?;
    super::view_rewrite::validate_public_merge_contract(rewrite, plan, &source)?;
    super::returning::validate_returning_alias_relations(
        &plan.target_qualifier,
        &plan.returning_aliases,
        Some(&source),
    )?;
    let target_schema = crate::RowSchema::with_qualified_types(
        &plan.target_qualifier,
        target.columns.clone(),
        target.types.clone(),
    );
    super::merge::validate_merge_action_scopes(
        rewrite.catalog,
        plan,
        &target_schema,
        &source,
        params,
        bindings,
    )
}
