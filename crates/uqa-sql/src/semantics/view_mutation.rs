//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View mutation target identity, declared columns, assignment and MERGE validation.
use super::view_rewrite::context::ViewRewriteContext;
use crate::{
    plan::{MergePlan, MergeWhenPlan},
    SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

mod production;

/// A captured mutation target retains only the definition used to read rows and its public identity.
#[derive(Debug)]
pub struct ViewMutationTarget {
    pub schema: crate::RowSchema,
    pub canonical_name: String,
    pub query: crate::plan::QueryPlan,
    pub security: Produced<crate::catalog::security::table_binding::CapturedTablePrivileges>,
    pub security_invoker: bool,
    pub columns: Vec<String>,
}

pub fn resolve_view_target(
    context: ViewRewriteContext<'_>,
    name: &str,
) -> Result<ViewMutationTarget, SQLError> {
    Ok(
        resolve_view_target_with_control(context, name, &ProductionControl::uncontrolled())?
            .into_uncontrolled()
            .expect("ordinary view mutation target"),
    )
}

pub fn resolve_view_target_with_control(
    context: ViewRewriteContext<'_>,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<ViewMutationTarget>, SQLError> {
    production::resolve(context, name, control)
}

pub fn target_columns(
    target: &ViewMutationTarget,
    explicit: &[crate::ast::AssignmentTarget<ScalarExpr>],
    operation: &str,
) -> Result<Vec<crate::ast::AssignmentTarget<ScalarExpr>>, SQLError> {
    let columns = if explicit.is_empty() {
        target.columns.iter().cloned().map(Into::into).collect()
    } else {
        explicit.to_vec()
    };
    crate::assignment::targets::validate_repeated_targets(&columns, operation == "INSERT")?;
    for column in &columns {
        if !target.columns.contains(&column.column) {
            return Err(SQLError::UnknownColumn(format!(
                "{}.{}",
                target.canonical_name, column.column
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

pub fn coerce_view_value(
    assignment: &dyn crate::assignment::AssignmentContext,
    target: &ViewMutationTarget,
    position: usize,
    value: Value,
) -> Result<Value, SQLError> {
    match target.schema.column_type(position) {
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
                    .flat_map(|assignment| assignment.target.targets().iter().cloned())
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
        rewrite.routines,
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
    let target_schema =
        crate::RowSchema::with_relation_qualifier(&target.schema, &plan.target_qualifier);
    super::merge::validate_merge_action_scopes(
        rewrite.routines,
        plan,
        &target_schema,
        &source,
        params,
        bindings,
    )
}
