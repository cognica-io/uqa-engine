//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public-view and mapped-target validation for automatic DML rewriting.

use super::{
    display_relation, instead_of_trigger_definition, non_writable_column,
    not_automatically_updatable, view_updatability, AutomaticViewLayer, BTreeSet,
    ConflictActionPlan, ConflictPlan, DeletePlan, Engine, InsertPlan, MergePlan, MergeWhenPlan,
    ReturningAliases, RowSchema, SQLError, ScalarExpr, TriggerEvent, UpdatePlan, ViewColumn,
    ViewMutationCapabilities,
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
    columns: impl IntoIterator<Item = &'a str>,
    duplicate: fn(&str) -> SQLError,
) -> Result<(), SQLError> {
    let mut seen = BTreeSet::new();
    for column in columns {
        if layer_column(layer, column).is_none() {
            return Err(unknown_view_column(layer, column));
        }
        if !seen.insert(column) {
            return Err(duplicate(column));
        }
    }
    Ok(())
}

pub(super) fn validate_mapped_columns(
    columns: &[String],
    duplicate: fn(&str) -> SQLError,
) -> Result<(), SQLError> {
    let mut seen = BTreeSet::new();
    for column in columns {
        if !seen.insert(column) {
            return Err(duplicate(column));
        }
    }
    Ok(())
}

pub(super) fn validate_insert_targets(
    layer: &AutomaticViewLayer,
    plan: &InsertPlan,
) -> Result<(), SQLError> {
    validate_view_target_columns(
        layer,
        plan.columns.iter().map(String::as_str),
        duplicate_insert_column,
    )?;
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
            assignments
                .iter()
                .map(|assignment| assignment.column.as_str()),
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
        plan.assignments
            .iter()
            .map(|assignment| assignment.column.as_str()),
        duplicate_assignment,
    )
}

pub(super) fn writable_column(
    layer: &AutomaticViewLayer,
    name: &str,
    operation: &str,
) -> Result<String, SQLError> {
    let column = layer_column(layer, name)
        .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", layer.canonical_name)))?;
    column
        .writable_source_column
        .clone()
        .ok_or_else(|| non_writable_column(&layer.canonical_name, name, operation))
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
    uqa_planner::rewrite_scalar_expression(&mut expression, &mut |node| {
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

fn public_view_columns(engine: &Engine, view: &str) -> Result<BTreeSet<String>, SQLError> {
    let definition = engine
        .view_definition(view)?
        .ok_or_else(|| SQLError::UnknownTable(view.to_string()))?;
    let schema = engine.stored_view_schema(&definition)?;
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
    uqa_planner::rewrite_scalar_expression(&mut expression, &mut |node| {
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
    engine: &Engine,
    plan: &UpdatePlan,
    source: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let columns = public_view_columns(engine, &plan.table)?;
    let ordinary_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source,
        include_excluded: false,
    };
    for assignment in &plan.assignments {
        validate_public_view_expression(&assignment.value, &columns, ordinary_scope)?;
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
    engine: &Engine,
    plan: &DeletePlan,
    source: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let columns = public_view_columns(engine, &plan.table)?;
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
    engine: &Engine,
    plan: &InsertPlan,
) -> Result<(), SQLError> {
    let columns = public_view_columns(engine, &plan.table)?;
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
            validate_public_view_expression(&assignment.value, &columns, scope)?;
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

pub(in crate::sql) fn validate_public_merge_contract(
    engine: &Engine,
    plan: &MergePlan,
    source: &RowSchema,
) -> Result<(), SQLError> {
    let columns = public_view_columns(engine, &plan.target)?;
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
                    validate_public_view_expression(&assignment.value, &columns, matched_scope)?;
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
                    assignments
                        .iter()
                        .map(|assignment| assignment.column.as_str()),
                    duplicate_assignment,
                )?;
            }
            MergeWhenPlan::InsertNotMatched { columns, .. } if !columns.is_empty() => {
                validate_view_target_columns(
                    layer,
                    columns.iter().map(String::as_str),
                    duplicate_insert_column,
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

pub(in crate::sql) fn validate_public_merge_targets(
    engine: &Engine,
    plan: &MergePlan,
) -> Result<(), SQLError> {
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                let columns = assignments
                    .iter()
                    .map(|assignment| assignment.column.as_str())
                    .collect::<Vec<_>>();
                validate_public_view_targets(engine, &plan.target, columns.iter().copied())?;
                validate_mapped_columns(
                    &columns
                        .iter()
                        .map(|column| (*column).to_string())
                        .collect::<Vec<_>>(),
                    duplicate_assignment,
                )?;
            }
            MergeWhenPlan::InsertNotMatched { columns, .. } if !columns.is_empty() => {
                validate_public_view_targets(
                    engine,
                    &plan.target,
                    columns.iter().map(String::as_str),
                )?;
                validate_mapped_columns(columns, duplicate_insert_column)?;
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn merge_action_capability_error(
    view: &str,
    clauses: &[MergeWhenPlan],
    capabilities: ViewMutationCapabilities,
) -> Option<SQLError> {
    clauses.iter().find_map(|clause| match clause {
        MergeWhenPlan::UpdateMatched { .. } | MergeWhenPlan::UpdateNotMatchedBySource { .. }
            if !capabilities.updatable =>
        {
            Some(not_automatically_updatable(view, "UPDATE"))
        }
        MergeWhenPlan::DeleteMatched { .. } | MergeWhenPlan::DeleteNotMatchedBySource { .. }
            if !capabilities.deletable =>
        {
            Some(not_automatically_updatable(view, "DELETE FROM"))
        }
        MergeWhenPlan::InsertNotMatched { .. } if !capabilities.insertable => {
            Some(not_automatically_updatable(view, "INSERT INTO"))
        }
        _ => None,
    })
}

fn validate_merge_rule_free(engine: &Engine, relation: &str) -> Result<(), SQLError> {
    let has_rules = [
        uqa_sql::ast::RuleEvent::Insert,
        uqa_sql::ast::RuleEvent::Update,
        uqa_sql::ast::RuleEvent::Delete,
    ]
    .into_iter()
    .map(|event| engine.rules_for(relation, event))
    .collect::<Result<Vec<_>, SQLError>>()?
    .iter()
    .any(|rules| !rules.is_empty());
    if !has_rules {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "0A000".into(),
        message: format!(
            "cannot execute MERGE on relation \"{}\"",
            display_relation(relation)
        ),
    })
}

fn merge_uses_event(plan: &MergePlan, event: TriggerEvent) -> bool {
    plan.when_clauses.iter().any(|clause| match event {
        TriggerEvent::Insert => matches!(clause, MergeWhenPlan::InsertNotMatched { .. }),
        TriggerEvent::Update => matches!(
            clause,
            MergeWhenPlan::UpdateMatched { .. } | MergeWhenPlan::UpdateNotMatchedBySource { .. }
        ),
        TriggerEvent::Delete => matches!(
            clause,
            MergeWhenPlan::DeleteMatched { .. } | MergeWhenPlan::DeleteNotMatchedBySource { .. }
        ),
        TriggerEvent::Truncate => false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::sql::dml) enum MergeViewTargetPath {
    AutomaticRewrite,
    ViewTriggers,
}

pub(in crate::sql::dml) fn merge_view_target_path(
    engine: &Engine,
    plan: &MergePlan,
) -> Result<MergeViewTargetPath, SQLError> {
    let canonical = engine
        .try_resolve_view_name(&plan.target)
        .map_err(|error| SQLError::Internal(format!("resolve MERGE view: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(plan.target.clone()))?;
    let definition = engine
        .view_definition(&canonical)?
        .ok_or_else(|| SQLError::UnknownTable(plan.target.clone()))?;
    if definition.kind == crate::StoredViewKind::Materialized {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!(
                "cannot execute MERGE on relation \"{}\"",
                display_relation(&canonical)
            ),
        });
    }
    validate_merge_rule_free(engine, &canonical)?;
    let automatic = view_updatability(engine, &canonical)?.automatic;
    let insert_trigger = instead_of_trigger_definition(engine, &canonical, TriggerEvent::Insert)?;
    let update_trigger = instead_of_trigger_definition(engine, &canonical, TriggerEvent::Update)?;
    let delete_trigger = instead_of_trigger_definition(engine, &canonical, TriggerEvent::Delete)?;
    let supported = ViewMutationCapabilities {
        insertable: automatic.insertable || insert_trigger,
        updatable: automatic.updatable || update_trigger,
        deletable: automatic.deletable || delete_trigger,
    };
    if let Some(error) = merge_action_capability_error(&canonical, &plan.when_clauses, supported) {
        return Err(error);
    }
    let mut uses_automatic = false;
    let mut uses_trigger = false;
    let mut has_action = false;
    for (event, trigger) in [
        (TriggerEvent::Insert, insert_trigger),
        (TriggerEvent::Update, update_trigger),
        (TriggerEvent::Delete, delete_trigger),
    ] {
        if !merge_uses_event(plan, event) {
            continue;
        }
        has_action = true;
        uses_trigger |= trigger;
        uses_automatic |= !trigger;
    }
    if uses_trigger && uses_automatic {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!(
                "cannot merge into view \"{}\"",
                display_relation(&canonical)
            ),
        });
    }
    if uses_trigger || !has_action {
        Ok(MergeViewTargetPath::ViewTriggers)
    } else {
        Ok(MergeViewTargetPath::AutomaticRewrite)
    }
}

pub(super) fn validate_public_view_targets<'a>(
    engine: &Engine,
    view: &str,
    columns: impl IntoIterator<Item = &'a str>,
) -> Result<(), SQLError> {
    let definition = engine
        .view_definition(view)?
        .ok_or_else(|| SQLError::UnknownTable(view.to_string()))?;
    let schema = engine.stored_view_schema(&definition)?;
    for column in columns {
        if !schema.has_unqualified_column(column) {
            return Err(SQLError::UnknownColumn(format!("{view}.{column}")));
        }
    }
    Ok(())
}

pub(super) fn validate_direct_view_rule_path(
    engine: &Engine,
    view: &str,
    event: uqa_sql::ast::RuleEvent,
    operation: &str,
) -> Result<(), SQLError> {
    let rules = engine.rules_for(view, event)?;
    let has_conditional_instead = rules
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_some());
    let has_unconditional_instead = rules
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none());
    if has_conditional_instead && !has_unconditional_instead {
        return Err(not_automatically_updatable(view, operation));
    }
    Ok(())
}
