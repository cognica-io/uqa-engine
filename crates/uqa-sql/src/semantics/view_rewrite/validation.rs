//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public-view and mapped-target validation for automatic DML rewriting.

use super::updatability::{non_writable_column, ColumnWrite, ViewCommand};
pub(super) mod production;

#[cfg(test)]
use super::BTreeSet;
use super::{
    AutomaticViewLayer, ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan,
    MergeWhenPlan, ReturningAliases, RowSchema, SQLError, ScalarExpr, TriggerEvent, UpdatePlan,
    ViewColumn, ViewRewriteContext,
};

#[cfg(test)]
mod tests;

pub(super) fn layer_column<'a>(
    layer: &'a AutomaticViewLayer,
    name: &str,
) -> Option<&'a ViewColumn> {
    layer.columns.iter().find(|column| column.name == name)
}

fn unknown_view_column(layer: &AutomaticViewLayer, column: &str) -> SQLError {
    SQLError::UnknownColumn(format!("{}.{column}", layer.canonical_name))
}

pub(super) fn duplicate_insert_column(column: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42701".into(),
        message: format!("column \"{column}\" specified more than once"),
    }
}

pub(super) fn duplicate_assignment(column: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: format!("multiple assignments to same column \"{column}\""),
    }
}

fn ambiguous_column(column: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42702".into(),
        message: format!("column reference \"{column}\" is ambiguous"),
    }
}

#[derive(Clone, Copy)]
pub(super) struct ExpressionScope<'a> {
    pub(super) target_qualifier: &'a str,
    pub(super) returning_aliases: Option<&'a ReturningAliases>,
    pub(super) source: Option<&'a RowSchema>,
    pub(super) include_excluded: bool,
}

impl ExpressionScope<'_> {
    pub(super) fn row_image_qualifier(self, qualifier: &str) -> bool {
        self.returning_aliases.is_some_and(|aliases| {
            [
                (aliases.old.as_str(), aliases.old_explicit),
                (aliases.new.as_str(), aliases.new_explicit),
            ]
            .into_iter()
            .any(|(alias, explicit)| {
                qualifier == alias
                    && (explicit
                        || !self
                            .source
                            .is_some_and(|source| source.has_qualifier(alias)))
            })
        })
    }

    pub(super) fn target_qualifier(self, qualifier: &str) -> bool {
        qualifier == self.target_qualifier
            || (self.include_excluded && qualifier == "excluded")
            || self.row_image_qualifier(qualifier)
    }
}

#[cfg(test)]
pub(super) fn validate_view_expression(
    expression: &ScalarExpr,
    layer: &AutomaticViewLayer,
    scope: ExpressionScope<'_>,
) -> Result<(), SQLError> {
    validate_reference_columns(
        expression,
        &|column| layer_column(layer, column).is_some(),
        scope,
    )
}

#[cfg(test)]
fn validate_reference_columns(
    expression: &ScalarExpr,
    contains: &dyn Fn(&str) -> bool,
    scope: ExpressionScope<'_>,
) -> Result<(), SQLError> {
    production::reference_columns(
        expression,
        contains,
        scope,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

#[cfg(test)]
fn validate_public_view_expression(
    expression: &ScalarExpr,
    public_columns: &BTreeSet<String>,
    scope: ExpressionScope<'_>,
) -> Result<(), SQLError> {
    validate_reference_columns(expression, &|column| public_columns.contains(column), scope)
}

pub fn validate_public_merge_contract(
    services: ViewRewriteContext<'_>,
    plan: &MergePlan,
    source: &RowSchema,
) -> Result<(), SQLError> {
    production::validate_public_merge_contract(
        services,
        plan,
        source,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub fn validate_public_merge_targets(
    services: ViewRewriteContext<'_>,
    plan: &MergePlan,
) -> Result<(), SQLError> {
    super::commands::validate_public_merge_targets_with_control(
        services,
        plan,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeViewTargetPath {
    AutomaticRewrite,
    ViewTriggers,
}

/// Choose how a MERGE reaches view `plan.target`, as `RewriteQuery` and `rewriteTargetView` do: through the view's INSTEAD OF triggers when every action has one, and otherwise by rewriting the view onto its base relation, which its query must allow, with an updatable column when an action inserts or updates. When it does not, the first action in WHEN order without a trigger reports why; an automatically updatable view cannot mix the two paths.
pub fn merge_view_target_path(
    services: ViewRewriteContext<'_>,
    plan: &MergePlan,
) -> Result<MergeViewTargetPath, SQLError> {
    super::commands::merge_view_target_path_with_control(
        services,
        plan,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

/// The trigger event and the view command of a MERGE action that writes, or `None` for `DO NOTHING`.
pub(super) fn merge_clause_command(clause: &MergeWhenPlan) -> Option<(TriggerEvent, ViewCommand)> {
    match clause {
        MergeWhenPlan::InsertNotMatched { .. } => {
            Some((TriggerEvent::Insert, ViewCommand::MergeInsert))
        }
        MergeWhenPlan::UpdateMatched { .. } | MergeWhenPlan::UpdateNotMatchedBySource { .. } => {
            Some((TriggerEvent::Update, ViewCommand::MergeUpdate))
        }
        MergeWhenPlan::DeleteMatched { .. } | MergeWhenPlan::DeleteNotMatchedBySource { .. } => {
            Some((TriggerEvent::Delete, ViewCommand::MergeDelete))
        }
        MergeWhenPlan::NothingMatched { .. }
        | MergeWhenPlan::NothingNotMatched { .. }
        | MergeWhenPlan::NothingNotMatchedBySource { .. } => None,
    }
}
