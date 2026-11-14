//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fixed, item, Child, Node};
use crate::ast::{
    AlterDomainAction, DeleteStmt, FromClause, InsertStmt, MergeStmt, MergeWhen, OnConflictAction,
    SelectStmt, Statement, UpdateStmt,
};

#[expect(
    clippy::too_many_lines,
    reason = "exhaustive statement selection preserves the initial-open migration surface"
)]
pub(super) fn statement(value: &Statement, position: usize) -> Child<'_> {
    match value {
        Statement::Select(value) => fixed([Some(Node::Select(value))], position),
        Statement::Insert(value) => fixed([Some(Node::Insert(value))], position),
        Statement::Update(value) => fixed([Some(Node::Update(value))], position),
        Statement::Delete(value) => fixed([Some(Node::Delete(value))], position),
        Statement::Merge(value) => fixed([Some(Node::Merge(value))], position),
        Statement::CreateFunction(value) => fixed([Some(Node::Function(value))], position),
        Statement::CreateDomain(value) => {
            if position == 0 {
                fixed([value.default.as_ref().map(Node::Expression)], 0)
            } else {
                item(&value.checks, position - 1, |check| {
                    Node::Expression(&check.expression)
                })
            }
        }
        Statement::AlterDomain(value) => match &value.action {
            AlterDomainAction::SetDefault { expression } => {
                fixed([expression.as_deref().map(Node::Expression)], position)
            }
            AlterDomainAction::AddCheck { constraint } => {
                fixed([Some(Node::Expression(&constraint.expression))], position)
            }
            AlterDomainAction::AddNotNull { .. }
            | AlterDomainAction::DropConstraint { .. }
            | AlterDomainAction::ValidateConstraint { .. } => Child::Done,
        },
        Statement::CreateView { body, .. }
        | Statement::CreateMaterializedView { body, .. }
        | Statement::CreateTableAs { body, .. } => fixed([Some(Node::Select(body))], position),
        Statement::DeclareCursor(cursor) => fixed([Some(Node::Select(&cursor.query))], position),
        Statement::Explain { body, .. } | Statement::Prepare { body, .. } => {
            fixed([Some(Node::Statement(body))], position)
        }
        Statement::Execute { params, .. } | Statement::Call { args: params, .. } => {
            fixed([Some(Node::Expressions(params))], position)
        }
        Statement::Values { rows } => fixed([Some(Node::Rows(rows))], position),
        Statement::CreateTrigger(trigger) => {
            fixed([trigger.when.as_ref().map(Node::Expression)], position)
        }
        Statement::CreateRule(rule) => fixed(
            [
                rule.condition.as_ref().map(Node::Expression),
                Some(Node::Statements(&rule.actions)),
            ],
            position,
        ),
        Statement::CreateEnum(_)
        | Statement::CreateCompositeType(_)
        | Statement::AlterEnum(_)
        | Statement::AlterTypeObject(_)
        | Statement::GrantType(_)
        | Statement::CommentType(_)
        | Statement::CreateShellType(_)
        | Statement::CreateTable(_)
        | Statement::CreateTableIfNotExists(_)
        | Statement::CreateIndex(_)
        | Statement::RenameIndex(_)
        | Statement::Drop(_)
        | Statement::AlterTable(_)
        | Statement::AlterForeignTable(_)
        | Statement::AlterView(_)
        | Statement::RefreshMaterializedView { .. }
        | Statement::CreateSchema { .. }
        | Statement::AlterSchemaOwner { .. }
        | Statement::RenameSchema { .. }
        | Statement::Notify { .. }
        | Statement::Listen { .. }
        | Statement::Unlisten { .. }
        | Statement::SetVariable { .. }
        | Statement::ResetVariable { .. }
        | Statement::ResetAllVariables
        | Statement::SetConstraints { .. }
        | Statement::ShowVariable { .. }
        | Statement::Discard { .. }
        | Statement::Load { .. }
        | Statement::Analyze { .. }
        | Statement::Vacuum(_)
        | Statement::LockTable(_)
        | Statement::Truncate { .. }
        | Statement::Transaction(_)
        | Statement::FetchCursor(_)
        | Statement::CloseCursor { .. }
        | Statement::CreateSequence(_)
        | Statement::AlterSequence(_)
        | Statement::Deallocate { .. }
        | Statement::CreateForeignWrapper(_)
        | Statement::CreateForeignServer(_)
        | Statement::CreateForeignTable(_)
        | Statement::CreateForeignTableDefinition(_)
        | Statement::DropFunction(_)
        | Statement::AlterRoutine(_)
        | Statement::AlterRoutineOwner(_)
        | Statement::RenameRoutine(_)
        | Statement::GrantRoutine(_)
        | Statement::GrantTable(_)
        | Statement::GrantSequence(_)
        | Statement::GrantDatabase(_)
        | Statement::GrantSchema(_)
        | Statement::GrantRole(_)
        | Statement::CreateRole(_)
        | Statement::AlterRole(_)
        | Statement::RenameRole(_)
        | Statement::DropRole(_)
        | Statement::DropTrigger(_)
        | Statement::DropRule(_)
        | Statement::DoBlock { .. }
        | Statement::Empty => Child::Done,
    }
}

pub(super) fn select(value: &SelectStmt, position: usize) -> Child<'_> {
    fixed(
        [
            Some(Node::Projections(&value.projections)),
            Some(Node::Windows(&value.windows)),
            Some(Node::Rows(&value.values)),
            value.from.as_ref().map(Node::From),
            value.r#where.as_ref().map(Node::Expression),
            Some(Node::Expressions(&value.group_by)),
            Some(Node::Rows(&value.grouping_sets)),
            value.having.as_ref().map(Node::Expression),
            Some(Node::Orders(&value.order_by)),
            value.limit.as_ref().map(Node::Expression),
            value.offset.as_ref().map(Node::Expression),
            Some(Node::Ctes(&value.with)),
            value
                .set_op
                .as_ref()
                .and_then(|set| set.left.as_deref())
                .map(Node::Select),
            value.set_op.as_ref().map(|set| Node::Select(&set.right)),
            value
                .set_op
                .as_ref()
                .map(|set| Node::Orders(&set.combined_order_by)),
            value
                .set_op
                .as_ref()
                .and_then(|set| set.combined_limit.as_ref())
                .map(Node::Expression),
            value
                .set_op
                .as_ref()
                .and_then(|set| set.combined_offset.as_ref())
                .map(Node::Expression),
            Some(Node::Expressions(&value.distinct_on)),
        ],
        position,
    )
}

pub(super) fn source(value: &FromClause, position: usize) -> Child<'_> {
    match value {
        FromClause::RetainedRows(_) | FromClause::Table { .. } => Child::Done,
        FromClause::Join {
            left, right, on, ..
        } => fixed(
            [
                Some(Node::From(left)),
                Some(Node::From(right)),
                on.as_ref().map(Node::Expression),
            ],
            position,
        ),
        FromClause::Values { rows, .. } => fixed([Some(Node::Rows(rows))], position),
        FromClause::Function { args, .. } => fixed([Some(Node::Expressions(args))], position),
        FromClause::FunctionGroup { functions, .. } => {
            fixed([Some(Node::TableFunctions(functions))], position)
        }
        FromClause::Subquery { body, .. } => fixed([Some(Node::Select(body))], position),
    }
}

pub(super) fn insert(value: &InsertStmt, position: usize) -> Child<'_> {
    let conflict = value.on_conflict.as_ref();
    let (assignments, condition) = match conflict.map(|conflict| &conflict.action) {
        Some(OnConflictAction::Update {
            assignments,
            r#where,
        }) => (
            Some(Node::Assignments(assignments)),
            r#where.as_deref().map(Node::Expression),
        ),
        _ => (None, None),
    };
    fixed(
        [
            Some(Node::Ctes(&value.with)),
            Some(Node::Targets(&value.columns)),
            Some(Node::Rows(&value.rows)),
            value.select_source.as_deref().map(Node::Select),
            conflict.map(|conflict| Node::Expressions(&conflict.expressions)),
            conflict
                .and_then(|conflict| conflict.predicate.as_deref())
                .map(Node::Expression),
            assignments,
            condition,
            Some(Node::Projections(&value.returning)),
        ],
        position,
    )
}

pub(super) fn update(value: &UpdateStmt, position: usize) -> Child<'_> {
    fixed(
        [
            Some(Node::Assignments(&value.assignments)),
            value.r#where.as_ref().map(Node::Expression),
            Some(Node::Ctes(&value.with)),
            value.from.as_ref().map(Node::From),
            Some(Node::Projections(&value.returning)),
        ],
        position,
    )
}

pub(super) fn delete(value: &DeleteStmt, position: usize) -> Child<'_> {
    fixed(
        [
            value.r#where.as_ref().map(Node::Expression),
            Some(Node::Ctes(&value.with)),
            value.using.as_ref().map(Node::From),
            Some(Node::Projections(&value.returning)),
        ],
        position,
    )
}

pub(super) fn merge(value: &MergeStmt, position: usize) -> Child<'_> {
    fixed(
        [
            Some(Node::Ctes(&value.with)),
            Some(Node::From(&value.source)),
            Some(Node::Expression(&value.join_condition)),
            Some(Node::Whens(&value.when_clauses)),
            Some(Node::Projections(&value.returning)),
        ],
        position,
    )
}

pub(super) fn when(value: &MergeWhen, position: usize) -> Child<'_> {
    match value {
        MergeWhen::UpdateMatched {
            condition,
            assignments,
        }
        | MergeWhen::UpdateNotMatchedBySource {
            condition,
            assignments,
        } => fixed(
            [
                condition.as_ref().map(Node::Expression),
                Some(Node::Assignments(assignments)),
            ],
            position,
        ),
        MergeWhen::InsertNotMatched {
            condition,
            columns,
            values,
            ..
        } => fixed(
            [
                condition.as_ref().map(Node::Expression),
                Some(Node::Targets(columns)),
                Some(Node::Expressions(values)),
            ],
            position,
        ),
        MergeWhen::DeleteMatched { condition }
        | MergeWhen::DeleteNotMatchedBySource { condition }
        | MergeWhen::NothingMatched { condition }
        | MergeWhen::NothingNotMatched { condition }
        | MergeWhen::NothingNotMatchedBySource { condition } => {
            fixed([condition.as_ref().map(Node::Expression)], position)
        }
    }
}
