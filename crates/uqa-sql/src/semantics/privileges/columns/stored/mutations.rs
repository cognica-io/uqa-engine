//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored mutations contribute written target columns and read sources under their original scopes.

use super::super::super::{
    analyze_query_plan, analyze_source_lineage, collect_expression_and_subqueries,
    lineage::{join_lineage, relation_lineage},
    ownership::{insert as insert_named, scopes},
    CteScope, QueryPlan, Required, SourceLineage,
};
use crate::{
    plan::{CommandPlan, ConflictActionPlan, MergeWhenPlan},
    SQLError, ScalarExpr,
};

pub(super) fn analyze(
    command: &CommandPlan,
    parent: &CteScope<'_, impl AsRef<[QueryPlan]>>,
    outer: &[&SourceLineage],
    universe: &mut SourceLineage,
    required: &mut Required,
) -> Result<(), SQLError> {
    let scope = super::ctes(command.ctes(), parent, outer, universe, required)?;
    let control = scope.control();
    if matches!(command, CommandPlan::Notify { .. }) {
        return Ok(());
    }
    let (target, local) = sources(command, &scope, outer, universe, required)?;
    let local_scopes = scopes(&local, outer, control)?;
    let read = |expression: &ScalarExpr,
                visible: &[&SourceLineage],
                required: &mut Required,
                universe: &mut SourceLineage| {
        collect_expression_and_subqueries(
            expression,
            visible,
            false,
            command.scalar_subqueries(),
            &scope,
            universe,
            required,
        )
    };
    match command {
        CommandPlan::Insert(command) => {
            insert(command, &target, outer, &scope, universe, required)?;
        }
        CommandPlan::Update(update) => {
            for assignment in &update.assignments {
                for column in assignment.target.targets() {
                    target.require_unqualified(&column.column, required, &control)?;
                }
                for expression in assignment.expressions() {
                    read(expression, &local_scopes, required, universe)?;
                }
            }
            if let Some(expression) = &update.predicate {
                read(expression, &local_scopes, required, universe)?;
            }
        }
        CommandPlan::Delete(delete) => {
            if let Some(expression) = &delete.predicate {
                read(expression, &local_scopes, required, universe)?;
            }
        }
        CommandPlan::Merge(command) => {
            merge(command, &local_scopes, &scope, universe, required)?;
        }
        _ => unreachable!("selected mutation"),
    }
    let mut returning = local.copy(&control)?;
    let aliases = command
        .returning_aliases()
        .expect("mutation returning aliases");
    for (alias, explicit) in [
        (&aliases.old, aliases.old_explicit),
        (&aliases.new, aliases.new_explicit),
    ] {
        if matches!(command, CommandPlan::Insert(_)) || explicit {
            insert_named(
                &mut returning.qualifiers,
                alias,
                target.output.copy(&control)?,
                &control,
            )?;
        }
    }
    let visible = scopes(&returning, outer, control)?;
    for projection in command.returning().expect("mutation returning") {
        collect_expression_and_subqueries(
            &projection.expr,
            &visible,
            true,
            command.scalar_subqueries(),
            &scope,
            universe,
            required,
        )?;
    }
    Ok(())
}

fn insert(
    insert: &crate::plan::InsertPlan,
    target: &SourceLineage,
    outer: &[&SourceLineage],
    scope: &CteScope<'_, impl AsRef<[QueryPlan]>>,
    universe: &mut SourceLineage,
    required: &mut Required,
) -> Result<(), SQLError> {
    let control = scope.control();
    let read = |expression: &ScalarExpr,
                visible: &[&SourceLineage],
                required: &mut Required,
                universe: &mut SourceLineage| {
        collect_expression_and_subqueries(
            expression,
            visible,
            false,
            &insert.subqueries,
            scope,
            universe,
            required,
        )
    };
    for expression in insert.rows.iter().flatten() {
        read(expression, outer, required, universe)?;
    }
    if let Some(query) = &insert.source {
        analyze_query_plan(query, scope, outer, universe, required)?;
    }
    for column in &insert.columns {
        target.require_unqualified(&column.column, required, &control)?;
        for expression in column.expressions() {
            read(expression, outer, required, universe)?;
        }
    }
    if let Some(conflict) = &insert.on_conflict {
        let mut conflict_scope = target.copy(&control)?;
        insert_named(
            &mut conflict_scope.qualifiers,
            "excluded",
            target.output.copy(&control)?,
            &control,
        )?;
        let visible = scopes(&conflict_scope, outer, control)?;
        for column in &conflict.conflict_columns {
            target.require_unqualified(column, required, &control)?;
        }
        for expression in conflict
            .expressions
            .iter()
            .chain(conflict.predicate.as_deref())
        {
            read(expression, &visible, required, universe)?;
        }
        if let ConflictActionPlan::Update {
            assignments,
            predicate,
        } = &conflict.action
        {
            for assignment in assignments {
                for column in assignment.target.targets() {
                    target.require_unqualified(&column.column, required, &control)?;
                }
                for expression in assignment.expressions() {
                    read(expression, &visible, required, universe)?;
                }
            }
            if let Some(expression) = predicate {
                read(expression, &visible, required, universe)?;
            }
        }
    }
    Ok(())
}

fn merge(
    merge: &crate::plan::MergePlan,
    local_scopes: &[&SourceLineage],
    scope: &CteScope<'_, impl AsRef<[QueryPlan]>>,
    universe: &mut SourceLineage,
    required: &mut Required,
) -> Result<(), SQLError> {
    let read = |expression: &ScalarExpr,
                visible: &[&SourceLineage],
                required: &mut Required,
                universe: &mut SourceLineage| {
        collect_expression_and_subqueries(
            expression,
            visible,
            false,
            &merge.subqueries,
            scope,
            universe,
            required,
        )
    };
    read(&merge.join_condition, local_scopes, required, universe)?;
    for action in &merge.when_clauses {
        let condition = match action {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            }
            | MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                // MERGE retains target assignments by attribute identity independently of its read-column dependencies.
                for assignment in assignments {
                    for expression in assignment.expressions() {
                        read(expression, local_scopes, required, universe)?;
                    }
                }
                condition
            }
            MergeWhenPlan::InsertNotMatched {
                condition,
                columns,
                values,
                ..
            } => {
                for expression in columns
                    .iter()
                    .flat_map(crate::ast::AssignmentTarget::expressions)
                    .chain(values)
                {
                    read(expression, local_scopes, required, universe)?;
                }
                condition
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingMatched { condition }
            | MergeWhenPlan::NothingNotMatched { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => condition,
        };
        if let Some(expression) = condition {
            read(expression, local_scopes, required, universe)?;
        }
    }
    Ok(())
}

fn sources(
    command: &CommandPlan,
    scope: &CteScope<'_, impl AsRef<[QueryPlan]>>,
    outer: &[&SourceLineage],
    universe: &mut SourceLineage,
    required: &mut Required,
) -> Result<(SourceLineage, SourceLineage), SQLError> {
    let control = scope.control();
    let table = command.mutation_target().ok_or_else(|| {
        SQLError::Internal("stored column dependencies require a query or rule action".into())
    })?;
    let relation = scope
        .catalog
        .relation_with_control(&scope.resolution, table, &control)?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    let qualifier = match command {
        CommandPlan::Merge(merge) => merge
            .target_alias
            .as_deref()
            .unwrap_or(&merge.target_qualifier),
        _ => command
            .target_qualifier()
            .expect("mutation target qualifier"),
    };
    let target = relation_lineage(&relation, [qualifier], &[], &control)?;
    let source_outer = scopes(&target, outer, control)?;
    let local = if let Some(source) = command.source_input() {
        let source = analyze_source_lineage(
            source,
            &source_outer,
            command.scalar_subqueries(),
            scope,
            universe,
            required,
        )?;
        join_lineage(
            target.copy(&control)?,
            source,
            None,
            false,
            None,
            &[],
            &control,
        )?
    } else {
        target.copy(&control)?
    };
    Ok((target, local))
}
