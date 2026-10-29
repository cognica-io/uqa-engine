//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command scalar transformation retains original analysis addresses and transfers each constructor's independent owner.

use super::super::nodes::{scalar_memory, transform_ctes, transform_query, transform_source};
use super::{CommandPlan, MutableCommandPlan, RewriteMemory, SQLError};
use crate::plan::ScalarNodeTransform;
use crate::{
    ast::{AssignmentStep, AssignmentTarget, AssignmentTargets},
    plan::{
        AssignmentPlan, ConflictActionPlan, ExpressionPlan, MergeWhenPlan, MutableExecutablePlan,
        OwnedScalarExpr, ProjectionPlan, QueryPlan, UnifiedPlan, ViewCheckPlan,
    },
    ScalarExpr,
};
use uqa_core::memory::ProductionControl;

impl MutableExecutablePlan {
    /// Transform scalar nodes at their original analysis identities, with children before parents and the same query/command order as immutable plan inspection.
    pub fn transform_scalar_nodes(
        self,
        control: &ProductionControl<'_>,
        transform: &mut ScalarNodeTransform<'_>,
    ) -> Result<Self, SQLError> {
        let mut plan = self;
        plan.transform_scalar_nodes_in_place(control, transform)?;
        Ok(plan)
    }
}

impl MutableExecutablePlan {
    /// Complete identity-indexed analysis before moving the inline query or command root.
    pub(crate) fn transform_scalar_nodes_in_place(
        &mut self,
        control: &ProductionControl<'_>,
        transform: &mut ScalarNodeTransform<'_>,
    ) -> Result<(), SQLError> {
        match self {
            Self::Query(query) => {
                query.assert_owner(control);
                super::super::nodes::transform_query(
                    &mut query.query,
                    query.memory.as_mut(),
                    control,
                    transform,
                )
            }
            Self::Command(command) => {
                command.assert_owner(control);
                transform_command(
                    &mut command.command,
                    command.memory.as_mut(),
                    control,
                    transform,
                )
            }
        }
    }
}

impl MutableCommandPlan {
    pub fn transform_scalar_nodes(
        mut self,
        control: &ProductionControl<'_>,
        transform: &mut ScalarNodeTransform<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        transform_command(&mut self.command, self.memory.as_mut(), control, transform)?;
        Ok(self)
    }
}

fn field<'a>(memory: Option<&'a mut RewriteMemory>, name: &str) -> Option<&'a mut RewriteMemory> {
    memory.map(|memory| {
        memory
            .field_mut(name)
            .as_mut()
            .expect("command constructor field")
    })
}

fn item(memory: Option<&mut RewriteMemory>, index: usize) -> Option<&mut RewriteMemory> {
    memory.map(|memory| {
        memory
            .item_mut(index)
            .as_mut()
            .expect("command constructor item")
    })
}

fn scalar(
    expression: &mut ScalarExpr,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    let memory = memory.map(|memory| scalar_memory(memory.scalar_mut()));
    OwnedScalarExpr::transform_nodes_in_place(expression, memory, control, transform)
}

fn optional(
    expression: Option<&mut ScalarExpr>,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    if let Some(expression) = expression {
        scalar(expression, memory, control, transform)?;
    }
    Ok(())
}

fn projections(
    values: &mut [ProjectionPlan],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    for (index, value) in values.iter_mut().enumerate() {
        scalar(
            &mut value.expr,
            item(memory.as_deref_mut(), index),
            control,
            transform,
        )?;
    }
    Ok(())
}

fn queries(
    values: &mut [QueryPlan],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    for (index, value) in values.iter_mut().enumerate() {
        transform_query(
            value,
            field(item(memory.as_deref_mut(), index), "query"),
            control,
            transform,
        )?;
    }
    Ok(())
}

fn checks(
    values: &mut [ViewCheckPlan],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    for (index, value) in values.iter_mut().enumerate() {
        scalar(
            &mut value.predicate,
            field(item(memory.as_deref_mut(), index), "predicate"),
            control,
            transform,
        )?;
    }
    Ok(())
}

fn target(
    value: &mut AssignmentTarget<ScalarExpr>,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    for (index, step) in value.indirection.iter_mut().enumerate() {
        let mut memory = item(field(memory.as_deref_mut(), "indirection"), index);
        match step {
            AssignmentStep::Field(_) => {}
            AssignmentStep::Index(expression) => {
                scalar(expression, field(memory, "index"), control, transform)?;
            }
            AssignmentStep::Slice { lower, upper } => {
                optional(
                    lower.as_deref_mut(),
                    field(memory.as_deref_mut(), "lower"),
                    control,
                    transform,
                )?;
                optional(
                    upper.as_deref_mut(),
                    field(memory, "upper"),
                    control,
                    transform,
                )?;
            }
        }
    }
    Ok(())
}

fn targets(
    values: &mut [AssignmentTarget<ScalarExpr>],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    for (index, value) in values.iter_mut().enumerate() {
        target(
            value,
            item(memory.as_deref_mut(), index),
            control,
            transform,
        )?;
    }
    Ok(())
}

fn assignments(
    values: &mut [AssignmentPlan],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    for (index, value) in values.iter_mut().enumerate() {
        let mut memory = item(memory.as_deref_mut(), index);
        let target_memory = field(memory.as_deref_mut(), "target");
        match &mut value.target {
            AssignmentTargets::Single(value) => target(value, target_memory, control, transform)?,
            AssignmentTargets::Multiple(group) => targets(
                &mut group.targets,
                field(target_memory, "targets"),
                control,
                transform,
            )?,
        }
        scalar(&mut value.value, field(memory, "value"), control, transform)?;
    }
    Ok(())
}

fn expressions(
    values: &mut [ExpressionPlan],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    for (index, value) in values.iter_mut().enumerate() {
        let mut memory = item(memory.as_deref_mut(), index);
        scalar(&mut value.scalar, memory.as_deref_mut(), control, transform)?;
        queries(
            &mut value.subqueries,
            field(memory, "subqueries"),
            control,
            transform,
        )?;
    }
    Ok(())
}

fn executable(
    value: &mut UnifiedPlan,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    match value {
        UnifiedPlan::Query(query) => {
            transform_query(query, field(memory, "query"), control, transform)
        }
        UnifiedPlan::Command(command) => {
            transform_command(command, field(memory, "command"), control, transform)
        }
    }
}

pub(in crate::plan::mutable) fn transform_command(
    command: &mut CommandPlan,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    transform_command_scope(command, memory, control, transform, true)
}

/// The caller owns nested arenas and local source ordering; visit only this command's direct mutation roots.
pub(in crate::plan::mutable) fn transform_local_command(
    command: &mut CommandPlan,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    transform_command_scope(command, memory, control, transform, false)
}

fn cte_children(
    values: &mut [crate::plan::CtePlan],
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
    recursive: bool,
) -> Result<(), SQLError> {
    if recursive {
        transform_ctes(values, memory, control, transform)?;
    }
    Ok(())
}

fn query_children(
    values: &mut [QueryPlan],
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
    recursive: bool,
) -> Result<(), SQLError> {
    if recursive {
        queries(values, field(memory, "subqueries"), control, transform)?;
    }
    Ok(())
}

fn query_child(
    value: &mut QueryPlan,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
    recursive: bool,
) -> Result<(), SQLError> {
    if recursive {
        transform_query(value, memory, control, transform)?;
    }
    Ok(())
}

fn source_scope(
    value: &mut crate::plan::SourcePlan,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
    recursive: bool,
) -> Result<(), SQLError> {
    if recursive {
        transform_source(value, memory, control, transform)
    } else {
        Ok(())
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one exhaustive command dispatch preserves analysis rewrite order and utility exclusions"
)]
fn transform_command_scope(
    command: &mut CommandPlan,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
    recursive: bool,
) -> Result<(), SQLError> {
    control.check()?;
    if !recursive
        && !matches!(
            command,
            CommandPlan::Insert(_)
                | CommandPlan::Update(_)
                | CommandPlan::Delete(_)
                | CommandPlan::Merge(_)
        )
    {
        return Ok(());
    }
    match command {
        CommandPlan::Insert(plan) => {
            let mut memory = field(memory, "value0");
            targets(
                &mut plan.columns,
                field(memory.as_deref_mut(), "columns"),
                control,
                transform,
            )?;
            cte_children(
                &mut plan.ctes,
                field(memory.as_deref_mut(), "ctes"),
                control,
                transform,
                recursive,
            )?;
            for (row, values) in plan.rows.iter_mut().enumerate() {
                for (column, value) in values.iter_mut().enumerate() {
                    scalar(
                        value,
                        item(item(field(memory.as_deref_mut(), "rows"), row), column),
                        control,
                        transform,
                    )?;
                }
            }
            if let Some(source) = &mut plan.source {
                query_child(
                    source,
                    field(field(memory.as_deref_mut(), "source"), "query"),
                    control,
                    transform,
                    recursive,
                )?;
            }
            if let Some(conflict) = &mut plan.on_conflict {
                let mut conflict_memory = field(memory.as_deref_mut(), "on_conflict");
                for (index, expression) in conflict.expressions.iter_mut().enumerate() {
                    scalar(
                        expression,
                        item(field(conflict_memory.as_deref_mut(), "expressions"), index),
                        control,
                        transform,
                    )?;
                }
                optional(
                    conflict.predicate.as_deref_mut(),
                    field(conflict_memory.as_deref_mut(), "predicate"),
                    control,
                    transform,
                )?;
                if let ConflictActionPlan::Update {
                    assignments: values,
                    predicate,
                } = &mut conflict.action
                {
                    let mut action_memory = field(conflict_memory, "action");
                    assignments(
                        values,
                        field(action_memory.as_deref_mut(), "assignments"),
                        control,
                        transform,
                    )?;
                    optional(
                        predicate.as_deref_mut(),
                        field(action_memory, "predicate"),
                        control,
                        transform,
                    )?;
                }
            }
            projections(
                &mut plan.returning,
                field(memory.as_deref_mut(), "returning"),
                control,
                transform,
            )?;
            checks(
                &mut plan.view_checks,
                field(memory.as_deref_mut(), "view_checks"),
                control,
                transform,
            )?;
            query_children(&mut plan.subqueries, memory, control, transform, recursive)?;
        }
        CommandPlan::Update(plan) => {
            let mut memory = field(memory, "value0");
            cte_children(
                &mut plan.ctes,
                field(memory.as_deref_mut(), "ctes"),
                control,
                transform,
                recursive,
            )?;
            if let Some(source) = &mut plan.source {
                source_scope(
                    source,
                    field(field(memory.as_deref_mut(), "source"), "source"),
                    control,
                    transform,
                    recursive,
                )?;
            }
            assignments(
                &mut plan.assignments,
                field(memory.as_deref_mut(), "assignments"),
                control,
                transform,
            )?;
            optional(
                plan.predicate.as_mut(),
                field(memory.as_deref_mut(), "predicate"),
                control,
                transform,
            )?;
            projections(
                &mut plan.returning,
                field(memory.as_deref_mut(), "returning"),
                control,
                transform,
            )?;
            checks(
                &mut plan.view_checks,
                field(memory.as_deref_mut(), "view_checks"),
                control,
                transform,
            )?;
            query_children(&mut plan.subqueries, memory, control, transform, recursive)?;
        }
        CommandPlan::Delete(plan) => {
            let mut memory = field(memory, "value0");
            cte_children(
                &mut plan.ctes,
                field(memory.as_deref_mut(), "ctes"),
                control,
                transform,
                recursive,
            )?;
            if let Some(source) = &mut plan.source {
                source_scope(
                    source,
                    field(field(memory.as_deref_mut(), "source"), "source"),
                    control,
                    transform,
                    recursive,
                )?;
            }
            optional(
                plan.predicate.as_mut(),
                field(memory.as_deref_mut(), "predicate"),
                control,
                transform,
            )?;
            projections(
                &mut plan.returning,
                field(memory.as_deref_mut(), "returning"),
                control,
                transform,
            )?;
            query_children(&mut plan.subqueries, memory, control, transform, recursive)?;
        }
        CommandPlan::Merge(plan) => {
            let mut memory = field(memory, "value0");
            cte_children(
                &mut plan.ctes,
                field(memory.as_deref_mut(), "ctes"),
                control,
                transform,
                recursive,
            )?;
            source_scope(
                &mut plan.source,
                field(field(memory.as_deref_mut(), "source"), "source"),
                control,
                transform,
                recursive,
            )?;
            optional(
                plan.target_predicate.as_mut(),
                field(memory.as_deref_mut(), "target_predicate"),
                control,
                transform,
            )?;
            scalar(
                &mut plan.join_condition,
                field(memory.as_deref_mut(), "join_condition"),
                control,
                transform,
            )?;
            for (index, clause) in plan.when_clauses.iter_mut().enumerate() {
                let mut memory = item(field(memory.as_deref_mut(), "when_clauses"), index);
                match clause {
                    MergeWhenPlan::UpdateMatched {
                        condition,
                        assignments: values,
                    }
                    | MergeWhenPlan::UpdateNotMatchedBySource {
                        condition,
                        assignments: values,
                    } => {
                        optional(
                            condition.as_mut(),
                            field(memory.as_deref_mut(), "condition"),
                            control,
                            transform,
                        )?;
                        assignments(values, field(memory, "assignments"), control, transform)?;
                    }
                    MergeWhenPlan::InsertNotMatched {
                        condition,
                        columns,
                        values,
                        ..
                    } => {
                        optional(
                            condition.as_mut(),
                            field(memory.as_deref_mut(), "condition"),
                            control,
                            transform,
                        )?;
                        targets(
                            columns,
                            field(memory.as_deref_mut(), "columns"),
                            control,
                            transform,
                        )?;
                        for (index, value) in values.iter_mut().enumerate() {
                            scalar(
                                value,
                                item(field(memory.as_deref_mut(), "values"), index),
                                control,
                                transform,
                            )?;
                        }
                    }
                    MergeWhenPlan::DeleteMatched { condition }
                    | MergeWhenPlan::DeleteNotMatchedBySource { condition }
                    | MergeWhenPlan::NothingMatched { condition }
                    | MergeWhenPlan::NothingNotMatched { condition }
                    | MergeWhenPlan::NothingNotMatchedBySource { condition } => optional(
                        condition.as_mut(),
                        field(memory, "condition"),
                        control,
                        transform,
                    )?,
                }
            }
            projections(
                &mut plan.returning,
                field(memory.as_deref_mut(), "returning"),
                control,
                transform,
            )?;
            checks(
                &mut plan.view_checks,
                field(memory.as_deref_mut(), "view_checks"),
                control,
                transform,
            )?;
            query_children(&mut plan.subqueries, memory, control, transform, recursive)?;
        }
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => query_child(
            query,
            field(field(memory, "query"), "query"),
            control,
            transform,
            recursive,
        )?,
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            executable(body, field(memory, "body"), control, transform)?;
        }
        CommandPlan::Execute { params, .. } => {
            expressions(params, field(memory, "params"), control, transform)?;
        }
        CommandPlan::Call { args, .. } => {
            expressions(args, field(memory, "args"), control, transform)?;
        }
        CommandPlan::CreateTable(_)
        | CommandPlan::CreateTableIfNotExists(_)
        | CommandPlan::CreateIndex(_)
        | CommandPlan::RenameIndex(_)
        | CommandPlan::Drop(_)
        | CommandPlan::AlterTable(_)
        | CommandPlan::AlterForeignTable(_)
        | CommandPlan::AlterView(_)
        | CommandPlan::RefreshMaterializedView { .. }
        | CommandPlan::CreateSchema { .. }
        | CommandPlan::AlterSchemaOwner { .. }
        | CommandPlan::RenameSchema { .. }
        | CommandPlan::Notify { .. }
        | CommandPlan::Listen { .. }
        | CommandPlan::Unlisten { .. }
        | CommandPlan::SetVariable { .. }
        | CommandPlan::ResetVariable { .. }
        | CommandPlan::ResetAllVariables
        | CommandPlan::SetConstraints { .. }
        | CommandPlan::ShowVariable { .. }
        | CommandPlan::Discard { .. }
        | CommandPlan::Load { .. }
        | CommandPlan::Analyze { .. }
        | CommandPlan::Vacuum(_)
        | CommandPlan::LockTable(_)
        | CommandPlan::Truncate { .. }
        | CommandPlan::Transaction(_)
        | CommandPlan::FetchCursor(_)
        | CommandPlan::CloseCursor { .. }
        | CommandPlan::CreateSequence(_)
        | CommandPlan::CreateDomain(_)
        | CommandPlan::AlterDomain(_)
        | CommandPlan::CommentType(_)
        | CommandPlan::CreateShellType(_)
        | CommandPlan::CreateEnum(_)
        | CommandPlan::CreateCompositeType(_)
        | CommandPlan::AlterEnum(_)
        | CommandPlan::AlterTypeObject(_)
        | CommandPlan::GrantType(_)
        | CommandPlan::AlterSequence(_)
        | CommandPlan::Deallocate { .. }
        | CommandPlan::CreateForeignWrapper(_)
        | CommandPlan::CreateForeignServer(_)
        | CommandPlan::CreateForeignTable(_)
        | CommandPlan::CreateForeignTableDefinition(_)
        | CommandPlan::CreateFunction(_)
        | CommandPlan::DropFunction(_)
        | CommandPlan::AlterRoutine(_)
        | CommandPlan::AlterRoutineOwner(_)
        | CommandPlan::RenameRoutine(_)
        | CommandPlan::GrantRoutine(_)
        | CommandPlan::GrantTable(_)
        | CommandPlan::GrantSequence(_)
        | CommandPlan::GrantDatabase(_)
        | CommandPlan::GrantSchema(_)
        | CommandPlan::GrantRole(_)
        | CommandPlan::CreateRole(_)
        | CommandPlan::AlterRole(_)
        | CommandPlan::RenameRole(_)
        | CommandPlan::DropRole(_)
        | CommandPlan::CreateTrigger(_)
        | CommandPlan::DropTrigger(_)
        | CommandPlan::CreateRule(_)
        | CommandPlan::DropRule(_)
        | CommandPlan::DoBlock { .. }
        | CommandPlan::Empty => {}
    }
    control.check()?;
    Ok(())
}
