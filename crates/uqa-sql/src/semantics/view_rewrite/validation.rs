//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public-view and mapped-target validation for automatic DML rewriting.

use super::updatability::{
    merge_into_materialized_view, merge_with_rules, mixed_merge_paths, non_writable_column,
    view_not_updatable, ColumnWrite, NotUpdatableReason, ViewCommand,
};
use super::{
    instead_of_trigger_definition, view_query_shape, AutomaticViewLayer, BTreeSet,
    ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan, MergeWhenPlan,
    ReturningAliases, RowSchema, SQLError, ScalarExpr, TriggerEvent, UpdatePlan, ViewColumn,
    ViewQueryShape, ViewRewriteContext,
};

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

fn validate_view_target_columns<'a>(
    layer: &AutomaticViewLayer,
    targets: impl IntoIterator<Item = &'a crate::ast::AssignmentTarget<ScalarExpr>>,
    duplicate: fn(&str) -> SQLError,
) -> Result<(), SQLError> {
    let mut seen = std::collections::BTreeMap::new();
    for target in targets {
        if layer_column(layer, &target.column).is_none() {
            return Err(unknown_view_column(layer, &target.column));
        }
        let whole = target.is_whole_column();
        if seen
            .insert(&target.column, whole)
            .is_some_and(|previous| previous || whole)
        {
            return Err(duplicate(&target.column));
        }
    }
    Ok(())
}

pub(super) fn validate_mapped_columns(
    targets: &[crate::ast::AssignmentTarget<ScalarExpr>],
    duplicate: fn(&str) -> SQLError,
) -> Result<(), SQLError> {
    let mut seen = std::collections::BTreeMap::new();
    for target in targets {
        let whole = target.is_whole_column();
        if seen
            .insert(&target.column, whole)
            .is_some_and(|previous| previous || whole)
        {
            return Err(duplicate(&target.column));
        }
    }
    Ok(())
}

pub(super) fn validate_insert_targets(
    layer: &AutomaticViewLayer,
    plan: &InsertPlan,
) -> Result<(), SQLError> {
    validate_view_target_columns(layer, plan.columns.iter(), duplicate_insert_column)?;
    let Some(conflict) = &plan.on_conflict else {
        return Ok(());
    };
    for column in &conflict.conflict_columns {
        if layer_column(layer, column).is_none() {
            return Err(unknown_view_column(layer, column));
        }
    }
    if let ConflictActionPlan::Update { assignments, .. } = &conflict.action {
        validate_view_target_columns(
            layer,
            assignments.iter().map(|assignment| &assignment.target),
            duplicate_assignment,
        )?;
    }
    Ok(())
}

pub(super) fn validate_update_targets(
    layer: &AutomaticViewLayer,
    plan: &UpdatePlan,
) -> Result<(), SQLError> {
    validate_view_target_columns(
        layer,
        plan.assignments.iter().map(|assignment| &assignment.target),
        duplicate_assignment,
    )
}

pub(super) fn writable_column(
    layer: &AutomaticViewLayer,
    name: &str,
    write: ColumnWrite,
) -> Result<String, SQLError> {
    let column = layer_column(layer, name)
        .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", layer.canonical_name)))?;
    column.writable_source_column.clone().ok_or_else(|| {
        non_writable_column(
            &layer.canonical_name,
            name,
            write,
            layer.column_restriction(column),
        )
    })
}

/// Reject the first of the `modified` columns, in the view's column order, that the view cannot write, as `view_cols_are_auto_updatable` finds it for `rewriteTargetView`.
pub(super) fn validate_writable_columns<'a>(
    layer: &AutomaticViewLayer,
    modified: impl IntoIterator<Item = &'a str>,
    write: ColumnWrite,
) -> Result<(), SQLError> {
    let modified = modified.into_iter().collect::<BTreeSet<_>>();
    match layer.columns.iter().find(|column| {
        column.writable_source_column.is_none() && modified.contains(column.name.as_str())
    }) {
        Some(column) => Err(non_writable_column(
            &layer.canonical_name,
            &column.name,
            write,
            layer.column_restriction(column),
        )),
        None => Ok(()),
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

pub(super) fn validate_view_expression(
    expression: &ScalarExpr,
    layer: &AutomaticViewLayer,
    scope: ExpressionScope<'_>,
) -> Result<(), SQLError> {
    let mut expression = expression.clone();
    let mut error = None;
    crate::plan::rewrite_scalar_expression(&mut expression, &mut |node| {
        if error.is_some() {
            return;
        }
        match node {
            ScalarExpr::Column(column) => {
                let target = layer_column(layer, column).is_some();
                let source = scope
                    .source
                    .is_some_and(|source| source.has_unqualified_column(column));
                if target && (source || scope.include_excluded) {
                    error = Some(ambiguous_column(column));
                } else if !target && !source {
                    error = Some(SQLError::UnknownColumn(column.clone()));
                }
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if scope.target_qualifier(qualifier) && layer_column(layer, column).is_none() =>
            {
                error = Some(SQLError::UnknownColumn(format!("{qualifier}.{column}")));
            }
            _ => {}
        }
    });
    error.map_or(Ok(()), Err)
}

fn public_view_columns(
    services: ViewRewriteContext<'_>,
    view: &str,
) -> Result<BTreeSet<String>, SQLError> {
    let definition = services
        .catalog
        .view_definition(view)?
        .ok_or_else(|| SQLError::UnknownTable(view.to_string()))?;
    let schema = stored_view_schema(services, &definition)?;
    Ok(schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| schema.public_name(position).unwrap_or(column).to_string())
        .collect())
}

fn validate_public_view_expression(
    expression: &ScalarExpr,
    public_columns: &BTreeSet<String>,
    scope: ExpressionScope<'_>,
) -> Result<(), SQLError> {
    let mut expression = expression.clone();
    let mut error = None;
    crate::plan::rewrite_scalar_expression(&mut expression, &mut |node| {
        if error.is_some() {
            return;
        }
        match node {
            ScalarExpr::Column(column) => {
                let target = public_columns.contains(column);
                let source = scope
                    .source
                    .is_some_and(|source| source.has_unqualified_column(column));
                if target && (source || scope.include_excluded) {
                    error = Some(ambiguous_column(column));
                } else if !target && !source {
                    error = Some(SQLError::UnknownColumn(column.clone()));
                }
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if scope.target_qualifier(qualifier) && !public_columns.contains(column) =>
            {
                error = Some(SQLError::UnknownColumn(format!("{qualifier}.{column}")));
            }
            _ => {}
        }
    });
    error.map_or(Ok(()), Err)
}

pub(super) fn validate_public_update_contract(
    services: ViewRewriteContext<'_>,
    plan: &UpdatePlan,
    source: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let columns = public_view_columns(services, &plan.table)?;
    let ordinary_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source,
        include_excluded: false,
    };
    for assignment in &plan.assignments {
        for expression in assignment.expressions() {
            validate_public_view_expression(expression, &columns, ordinary_scope)?;
        }
    }
    if let Some(predicate) = plan.predicate.as_ref() {
        validate_public_view_expression(predicate, &columns, ordinary_scope)?;
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..ordinary_scope
    };
    for projection in &plan.returning {
        validate_public_view_expression(&projection.expr, &columns, returning_scope)?;
    }
    Ok(())
}

pub(super) fn validate_public_delete_contract(
    services: ViewRewriteContext<'_>,
    plan: &DeletePlan,
    source: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let columns = public_view_columns(services, &plan.table)?;
    let ordinary_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source,
        include_excluded: false,
    };
    if let Some(predicate) = plan.predicate.as_ref() {
        validate_public_view_expression(predicate, &columns, ordinary_scope)?;
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..ordinary_scope
    };
    for projection in &plan.returning {
        validate_public_view_expression(&projection.expr, &columns, returning_scope)?;
    }
    Ok(())
}

pub(super) fn validate_public_insert_contract(
    services: ViewRewriteContext<'_>,
    plan: &InsertPlan,
) -> Result<(), SQLError> {
    let columns = public_view_columns(services, &plan.table)?;
    for predicate in plan.on_conflict.iter().flat_map(|conflict| {
        conflict
            .expressions
            .iter()
            .chain(conflict.predicate.iter().map(Box::as_ref))
    }) {
        validate_public_view_expression(
            predicate,
            &columns,
            ExpressionScope {
                target_qualifier: &plan.target_qualifier,
                returning_aliases: None,
                source: None,
                include_excluded: false,
            },
        )?;
    }
    if let Some(ConflictPlan {
        action:
            ConflictActionPlan::Update {
                assignments,
                predicate,
            },
        ..
    }) = &plan.on_conflict
    {
        let scope = ExpressionScope {
            target_qualifier: &plan.target_qualifier,
            returning_aliases: None,
            source: None,
            include_excluded: true,
        };
        for assignment in assignments {
            for expression in assignment.expressions() {
                validate_public_view_expression(expression, &columns, scope)?;
            }
        }
        if let Some(predicate) = predicate {
            validate_public_view_expression(predicate, &columns, scope)?;
        }
    }
    let scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: Some(&plan.returning_aliases),
        source: None,
        include_excluded: false,
    };
    for projection in &plan.returning {
        validate_public_view_expression(&projection.expr, &columns, scope)?;
    }
    Ok(())
}

pub fn validate_public_merge_contract(
    services: ViewRewriteContext<'_>,
    plan: &MergePlan,
    source: &RowSchema,
) -> Result<(), SQLError> {
    let columns = public_view_columns(services, &plan.target)?;
    let matched_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source: Some(source),
        include_excluded: false,
    };
    let target_only_scope = ExpressionScope {
        source: None,
        ..matched_scope
    };
    validate_public_view_expression(&plan.join_condition, &columns, matched_scope)?;
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => {
                if let Some(condition) = condition {
                    validate_public_view_expression(condition, &columns, matched_scope)?;
                }
                for assignment in assignments {
                    for expression in assignment.expressions() {
                        validate_public_view_expression(expression, &columns, matched_scope)?;
                    }
                }
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition } => {
                if let Some(condition) = condition {
                    validate_public_view_expression(condition, &columns, matched_scope)?;
                }
            }
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                if let Some(condition) = condition {
                    validate_public_view_expression(condition, &columns, target_only_scope)?;
                }
                for assignment in assignments {
                    validate_public_view_expression(
                        &assignment.value,
                        &columns,
                        target_only_scope,
                    )?;
                }
            }
            MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                if let Some(condition) = condition {
                    validate_public_view_expression(condition, &columns, target_only_scope)?;
                }
            }
            MergeWhenPlan::InsertNotMatched { .. } | MergeWhenPlan::NothingNotMatched { .. } => {}
        }
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..matched_scope
    };
    for projection in &plan.returning {
        validate_public_view_expression(&projection.expr, &columns, returning_scope)?;
    }
    Ok(())
}

pub(super) fn validate_merge_targets(
    layer: &AutomaticViewLayer,
    plan: &MergePlan,
) -> Result<(), SQLError> {
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                validate_view_target_columns(
                    layer,
                    assignments.iter().map(|assignment| &assignment.target),
                    duplicate_assignment,
                )?;
            }
            MergeWhenPlan::InsertNotMatched { columns, .. } if !columns.is_empty() => {
                validate_view_target_columns(layer, columns.iter(), duplicate_insert_column)?;
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn validate_public_merge_targets(
    services: ViewRewriteContext<'_>,
    plan: &MergePlan,
) -> Result<(), SQLError> {
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                validate_public_view_targets(
                    services,
                    &plan.target,
                    assignments
                        .iter()
                        .map(|assignment| assignment.target.column.as_str()),
                )?;
                validate_mapped_columns(
                    &assignments
                        .iter()
                        .map(|assignment| assignment.target.clone())
                        .collect::<Vec<_>>(),
                    duplicate_assignment,
                )?;
            }
            MergeWhenPlan::InsertNotMatched { columns, .. } if !columns.is_empty() => {
                validate_public_view_targets(
                    services,
                    &plan.target,
                    columns.iter().map(|target| target.column.as_str()),
                )?;
                validate_mapped_columns(columns, duplicate_insert_column)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_merge_rule_free(
    services: ViewRewriteContext<'_>,
    relation: &str,
) -> Result<(), SQLError> {
    let has_rules = [
        crate::ast::RuleEvent::Insert,
        crate::ast::RuleEvent::Update,
        crate::ast::RuleEvent::Delete,
    ]
    .into_iter()
    .map(|event| services.catalog.rules_for(relation, event))
    .collect::<Result<Vec<_>, SQLError>>()?
    .iter()
    .any(|rules| !rules.is_empty());
    if !has_rules {
        return Ok(());
    }
    Err(merge_with_rules(relation))
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
    let canonical = services
        .catalog
        .try_resolve_view_name(&plan.target)
        .map_err(|error| SQLError::Internal(format!("resolve MERGE view: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(plan.target.clone()))?;
    let definition = services
        .catalog
        .view_definition(&canonical)?
        .ok_or_else(|| SQLError::UnknownTable(plan.target.clone()))?;
    if definition.kind == crate::catalog::view::StoredViewKind::Materialized {
        return Err(merge_into_materialized_view(&canonical));
    }
    validate_merge_rule_free(services, &canonical)?;
    let mut untriggered = None;
    let mut triggered = false;
    for clause in &plan.when_clauses {
        let Some((event, command)) = merge_clause_command(clause) else {
            continue;
        };
        if instead_of_trigger_definition(services, &canonical, event)? {
            triggered = true;
        } else if untriggered.is_none() {
            untriggered = Some(command);
        }
    }
    let Some(untriggered) = untriggered else {
        return Ok(MergeViewTargetPath::ViewTriggers);
    };
    let writes_columns = plan.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::InsertNotMatched { .. }
                | MergeWhenPlan::UpdateMatched { .. }
                | MergeWhenPlan::UpdateNotMatchedBySource { .. }
        )
    });
    let reason = match view_query_shape(services, &canonical)? {
        Some(ViewQueryShape::Updatable(layer)) => (writes_columns && !layer.has_writable_column())
            .then_some(NotUpdatableReason::NoUpdatableColumns),
        Some(ViewQueryShape::NotUpdatable(reason)) => Some(reason),
        None => return Err(SQLError::UnknownTable(plan.target.clone())),
    };
    if let Some(reason) = reason {
        return Err(view_not_updatable(&canonical, untriggered, reason));
    }
    if triggered {
        return Err(mixed_merge_paths(&canonical));
    }
    Ok(MergeViewTargetPath::AutomaticRewrite)
}

/// The trigger event and the view command of a MERGE action that writes, or `None` for `DO NOTHING`.
fn merge_clause_command(clause: &MergeWhenPlan) -> Option<(TriggerEvent, ViewCommand)> {
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

pub(super) fn validate_public_view_targets<'a>(
    services: ViewRewriteContext<'_>,
    view: &str,
    columns: impl IntoIterator<Item = &'a str>,
) -> Result<(), SQLError> {
    let definition = services
        .catalog
        .view_definition(view)?
        .ok_or_else(|| SQLError::UnknownTable(view.to_string()))?;
    let schema = stored_view_schema(services, &definition)?;
    for column in columns {
        if !schema.has_unqualified_column(column) {
            return Err(SQLError::UnknownColumn(format!("{view}.{column}")));
        }
    }
    Ok(())
}

/// Reject a view whose rules for `event` are conditional INSTEAD rules without an unconditional one, as `RewriteQuery` does before it tries to rewrite the view.
pub(super) fn validate_direct_view_rule_path(
    services: ViewRewriteContext<'_>,
    view: &str,
    event: crate::ast::RuleEvent,
    command: ViewCommand,
) -> Result<(), SQLError> {
    let rules = services.catalog.rules_for(view, event)?;
    let has_conditional_instead = rules
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_some());
    let has_unconditional_instead = rules
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none());
    if has_conditional_instead && !has_unconditional_instead {
        return Err(view_not_updatable(
            view,
            command,
            NotUpdatableReason::ConditionalInsteadRule,
        ));
    }
    Ok(())
}

use super::stored_view_schema;
