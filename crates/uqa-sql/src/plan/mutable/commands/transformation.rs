//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command scalar transformation retains original analysis addresses and transfers each constructor's independent owner.

use super::super::nodes::transformation::{
    ctes_from,
    input::{checkpoint, Immediate, Input},
};
use super::{CommandPlan, MutableCommandPlan, RewriteMemory, SQLError};
use crate::plan::ScalarNodeTransform;
use crate::{
    ast::{AssignmentStep, AssignmentTarget, AssignmentTargets},
    plan::{
        AssignmentPlan, ConflictActionPlan, ExpressionPlan, MergeWhenPlan, MutableExecutablePlan,
        ProjectionPlan, QueryPlan, UnifiedPlan, ViewCheckPlan,
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

pub(in crate::plan::mutable) fn transform_command(
    command: &mut CommandPlan,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    transform: &mut ScalarNodeTransform<'_>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(command_from(
        command,
        memory,
        control,
        &mut Immediate::new(transform),
        true,
    ))
}

fn field<'a>(memory: Option<&'a mut RewriteMemory>, name: &str) -> Option<&'a mut RewriteMemory> {
    memory.map(|memory| {
        memory
            .field_mut(name)
            .as_mut()
            .expect("command constructor field")
    })
}

fn field_slot<'a>(
    memory: Option<&'a mut RewriteMemory>,
    name: &str,
) -> Option<&'a mut Option<RewriteMemory>> {
    memory.map(|memory| memory.field_mut(name))
}

fn item(memory: Option<&mut RewriteMemory>, index: usize) -> Option<&mut RewriteMemory> {
    memory.map(|memory| {
        memory
            .item_mut(index)
            .as_mut()
            .expect("command constructor item")
    })
}

async fn scalar<I: Input>(
    expression: &mut ScalarExpr,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    let memory = memory.map(RewriteMemory::scalar_mut);
    input.scalar(expression, memory, control).await
}

async fn optional<I: Input>(
    expression: Option<&mut ScalarExpr>,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    if let Some(expression) = expression {
        scalar(expression, memory, control, input).await?;
    }
    Ok(())
}

async fn projections<I: Input>(
    values: &mut [ProjectionPlan],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    for (index, value) in values.iter_mut().enumerate() {
        checkpoint(control, input).await?;
        scalar(
            &mut value.expr,
            item(memory.as_deref_mut(), index),
            control,
            input,
        )
        .await?;
    }
    Ok(())
}

async fn queries<I: Input>(
    values: &mut [QueryPlan],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    for (index, value) in values.iter_mut().enumerate() {
        checkpoint(control, input).await?;
        input
            .query(
                value,
                field_slot(item(memory.as_deref_mut(), index), "query"),
                control,
            )?
            .await?;
    }
    Ok(())
}

async fn checks<I: Input>(
    values: &mut [ViewCheckPlan],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    for (index, value) in values.iter_mut().enumerate() {
        checkpoint(control, input).await?;
        scalar(
            &mut value.predicate,
            field(item(memory.as_deref_mut(), index), "predicate"),
            control,
            input,
        )
        .await?;
    }
    Ok(())
}

async fn target<I: Input>(
    value: &mut AssignmentTarget<ScalarExpr>,
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    for (index, step) in value.indirection.iter_mut().enumerate() {
        checkpoint(control, input).await?;
        let mut memory = item(field(memory.as_deref_mut(), "indirection"), index);
        match step {
            AssignmentStep::Field(_) => {}
            AssignmentStep::Index(expression) => {
                scalar(expression, field(memory, "index"), control, input).await?;
            }
            AssignmentStep::Slice { lower, upper } => {
                optional(
                    lower.as_deref_mut(),
                    field(memory.as_deref_mut(), "lower"),
                    control,
                    input,
                )
                .await?;
                optional(upper.as_deref_mut(), field(memory, "upper"), control, input).await?;
            }
        }
    }
    Ok(())
}

async fn targets<I: Input>(
    values: &mut [AssignmentTarget<ScalarExpr>],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    for (index, value) in values.iter_mut().enumerate() {
        checkpoint(control, input).await?;
        target(value, item(memory.as_deref_mut(), index), control, input).await?;
    }
    Ok(())
}

async fn assignments<I: Input>(
    values: &mut [AssignmentPlan],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    for (index, value) in values.iter_mut().enumerate() {
        checkpoint(control, input).await?;
        let mut memory = item(memory.as_deref_mut(), index);
        let target_memory = field(memory.as_deref_mut(), "target");
        match &mut value.target {
            AssignmentTargets::Single(value) => {
                target(value, target_memory, control, input).await?;
            }
            AssignmentTargets::Multiple(group) => {
                targets(
                    &mut group.targets,
                    field(target_memory, "targets"),
                    control,
                    input,
                )
                .await?;
            }
        }
        scalar(&mut value.value, field(memory, "value"), control, input).await?;
    }
    Ok(())
}

async fn expressions<I: Input>(
    values: &mut [ExpressionPlan],
    mut memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    for (index, value) in values.iter_mut().enumerate() {
        checkpoint(control, input).await?;
        let mut memory = item(memory.as_deref_mut(), index);
        scalar(&mut value.scalar, memory.as_deref_mut(), control, input).await?;
        queries(
            &mut value.subqueries,
            field(memory, "subqueries"),
            control,
            input,
        )
        .await?;
    }
    Ok(())
}

async fn executable<I: Input>(
    value: &mut UnifiedPlan,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    match value {
        UnifiedPlan::Query(query) => {
            input
                .query(query, field_slot(memory, "query"), control)?
                .await
        }
        UnifiedPlan::Command(command) => {
            input
                .command(command, field_slot(memory, "command"), control)?
                .await
        }
    }
}

async fn cte_children<I: Input>(
    values: &mut [crate::plan::CtePlan],
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
    recursive: bool,
) -> Result<(), SQLError> {
    if recursive {
        ctes_from(values, memory, control, input).await?;
    }
    Ok(())
}

async fn query_children<I: Input>(
    values: &mut [QueryPlan],
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
    recursive: bool,
) -> Result<(), SQLError> {
    if recursive {
        queries(values, field(memory, "subqueries"), control, input).await?;
    }
    Ok(())
}

async fn query_child<I: Input>(
    value: &mut QueryPlan,
    memory: Option<&mut Option<RewriteMemory>>,
    control: &ProductionControl<'_>,
    input: &mut I,
    recursive: bool,
) -> Result<(), SQLError> {
    if recursive {
        input.query(value, memory, control)?.await?;
    }
    Ok(())
}

async fn source_scope<I: Input>(
    value: &mut crate::plan::SourcePlan,
    memory: Option<&mut Option<RewriteMemory>>,
    control: &ProductionControl<'_>,
    input: &mut I,
    recursive: bool,
) -> Result<(), SQLError> {
    if recursive {
        input.source(value, memory, control)?.await
    } else {
        Ok(())
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one exhaustive command dispatch preserves analysis rewrite order and utility exclusions"
)]
pub(in crate::plan::mutable) async fn command_from<I: Input>(
    command: &mut CommandPlan,
    memory: Option<&mut RewriteMemory>,
    control: &ProductionControl<'_>,
    input: &mut I,
    recursive: bool,
) -> Result<(), SQLError> {
    checkpoint(control, input).await?;
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
                input,
            )
            .await?;
            cte_children(
                &mut plan.ctes,
                field(memory.as_deref_mut(), "ctes"),
                control,
                input,
                recursive,
            )
            .await?;
            for (row, values) in plan.rows.iter_mut().enumerate() {
                checkpoint(control, input).await?;
                for (column, value) in values.iter_mut().enumerate() {
                    checkpoint(control, input).await?;
                    let memory = item(field(memory.as_deref_mut(), "rows"), row)
                        .map(|memory| memory.scalar_item_mut(column));
                    input.scalar(value, memory, control).await?;
                }
            }
            if let Some(source) = &mut plan.source {
                query_child(
                    source,
                    field_slot(field(memory.as_deref_mut(), "source"), "query"),
                    control,
                    input,
                    recursive,
                )
                .await?;
            }
            if let Some(conflict) = &mut plan.on_conflict {
                let mut conflict_memory = field(memory.as_deref_mut(), "on_conflict");
                for (index, expression) in conflict.expressions.iter_mut().enumerate() {
                    checkpoint(control, input).await?;
                    scalar(
                        expression,
                        item(field(conflict_memory.as_deref_mut(), "expressions"), index),
                        control,
                        input,
                    )
                    .await?;
                }
                optional(
                    conflict.predicate.as_deref_mut(),
                    field(conflict_memory.as_deref_mut(), "predicate"),
                    control,
                    input,
                )
                .await?;
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
                        input,
                    )
                    .await?;
                    optional(
                        predicate.as_deref_mut(),
                        field(action_memory, "predicate"),
                        control,
                        input,
                    )
                    .await?;
                }
            }
            projections(
                &mut plan.returning,
                field(memory.as_deref_mut(), "returning"),
                control,
                input,
            )
            .await?;
            checks(
                &mut plan.view_checks,
                field(memory.as_deref_mut(), "view_checks"),
                control,
                input,
            )
            .await?;
            query_children(&mut plan.subqueries, memory, control, input, recursive).await?;
        }
        CommandPlan::Update(plan) => {
            let mut memory = field(memory, "value0");
            cte_children(
                &mut plan.ctes,
                field(memory.as_deref_mut(), "ctes"),
                control,
                input,
                recursive,
            )
            .await?;
            if let Some(source) = &mut plan.source {
                source_scope(
                    source,
                    field_slot(field(memory.as_deref_mut(), "source"), "source"),
                    control,
                    input,
                    recursive,
                )
                .await?;
            }
            assignments(
                &mut plan.assignments,
                field(memory.as_deref_mut(), "assignments"),
                control,
                input,
            )
            .await?;
            optional(
                plan.predicate.as_mut(),
                field(memory.as_deref_mut(), "predicate"),
                control,
                input,
            )
            .await?;
            projections(
                &mut plan.returning,
                field(memory.as_deref_mut(), "returning"),
                control,
                input,
            )
            .await?;
            checks(
                &mut plan.view_checks,
                field(memory.as_deref_mut(), "view_checks"),
                control,
                input,
            )
            .await?;
            query_children(&mut plan.subqueries, memory, control, input, recursive).await?;
        }
        CommandPlan::Delete(plan) => {
            let mut memory = field(memory, "value0");
            cte_children(
                &mut plan.ctes,
                field(memory.as_deref_mut(), "ctes"),
                control,
                input,
                recursive,
            )
            .await?;
            if let Some(source) = &mut plan.source {
                source_scope(
                    source,
                    field_slot(field(memory.as_deref_mut(), "source"), "source"),
                    control,
                    input,
                    recursive,
                )
                .await?;
            }
            optional(
                plan.predicate.as_mut(),
                field(memory.as_deref_mut(), "predicate"),
                control,
                input,
            )
            .await?;
            projections(
                &mut plan.returning,
                field(memory.as_deref_mut(), "returning"),
                control,
                input,
            )
            .await?;
            query_children(&mut plan.subqueries, memory, control, input, recursive).await?;
        }
        CommandPlan::Merge(plan) => {
            let mut memory = field(memory, "value0");
            cte_children(
                &mut plan.ctes,
                field(memory.as_deref_mut(), "ctes"),
                control,
                input,
                recursive,
            )
            .await?;
            source_scope(
                &mut plan.source,
                field_slot(field(memory.as_deref_mut(), "source"), "source"),
                control,
                input,
                recursive,
            )
            .await?;
            optional(
                plan.target_predicate.as_mut(),
                field(memory.as_deref_mut(), "target_predicate"),
                control,
                input,
            )
            .await?;
            scalar(
                &mut plan.join_condition,
                field(memory.as_deref_mut(), "join_condition"),
                control,
                input,
            )
            .await?;
            for (index, clause) in plan.when_clauses.iter_mut().enumerate() {
                checkpoint(control, input).await?;
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
                            input,
                        )
                        .await?;
                        assignments(values, field(memory, "assignments"), control, input).await?;
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
                            input,
                        )
                        .await?;
                        targets(
                            columns,
                            field(memory.as_deref_mut(), "columns"),
                            control,
                            input,
                        )
                        .await?;
                        for (index, value) in values.iter_mut().enumerate() {
                            checkpoint(control, input).await?;
                            scalar(
                                value,
                                item(field(memory.as_deref_mut(), "values"), index),
                                control,
                                input,
                            )
                            .await?;
                        }
                    }
                    MergeWhenPlan::DeleteMatched { condition }
                    | MergeWhenPlan::DeleteNotMatchedBySource { condition }
                    | MergeWhenPlan::NothingMatched { condition }
                    | MergeWhenPlan::NothingNotMatched { condition }
                    | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                        optional(
                            condition.as_mut(),
                            field(memory, "condition"),
                            control,
                            input,
                        )
                        .await?;
                    }
                }
            }
            projections(
                &mut plan.returning,
                field(memory.as_deref_mut(), "returning"),
                control,
                input,
            )
            .await?;
            checks(
                &mut plan.view_checks,
                field(memory.as_deref_mut(), "view_checks"),
                control,
                input,
            )
            .await?;
            query_children(&mut plan.subqueries, memory, control, input, recursive).await?;
        }
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => {
            query_child(
                query,
                field_slot(field(memory, "query"), "query"),
                control,
                input,
                recursive,
            )
            .await?;
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            executable(body, field(memory, "body"), control, input).await?;
        }
        CommandPlan::Execute { params, .. } => {
            expressions(params, field(memory, "params"), control, input).await?;
        }
        CommandPlan::Call { args, .. } => {
            expressions(args, field(memory, "args"), control, input).await?;
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
        | CommandPlan::AlterForeignServer(_)
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
        | CommandPlan::DropOwned(_)
        | CommandPlan::CreateTrigger(_)
        | CommandPlan::DropTrigger(_)
        | CommandPlan::CreateRule(_)
        | CommandPlan::DropRule(_)
        | CommandPlan::DoBlock { .. }
        | CommandPlan::Empty => {}
    }
    checkpoint(control, input).await?;
    Ok(())
}
