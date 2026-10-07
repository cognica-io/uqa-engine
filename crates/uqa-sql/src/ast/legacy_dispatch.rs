//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore legacy function dispatch markers throughout stored statements.

use super::{
    Expr, FromClause, FunctionBody, MergeWhen, OnConflictAction, Projection, SelectStmt, Statement,
    CTE,
};

fn upgrade_exprs(expressions: &mut [Expr]) -> bool {
    expressions.iter_mut().fold(false, |changed, expression| {
        expression.upgrade_legacy_serialized_dispatches() | changed
    })
}

fn upgrade_rows(rows: &mut [Vec<Expr>]) -> bool {
    rows.iter_mut()
        .fold(false, |changed, row| upgrade_exprs(row) | changed)
}

fn upgrade_optional(expression: &mut Option<Expr>) -> bool {
    expression
        .as_mut()
        .is_some_and(Expr::upgrade_legacy_serialized_dispatches)
}

fn upgrade_projections(projections: &mut [Projection]) -> bool {
    projections.iter_mut().fold(false, |changed, projection| {
        projection.expr.upgrade_legacy_serialized_dispatches() | changed
    })
}

fn upgrade_targets(targets: &mut [super::AssignmentTarget]) -> bool {
    targets
        .iter_mut()
        .flat_map(super::AssignmentTarget::expressions_mut)
        .fold(false, |changed, expression| {
            expression.upgrade_legacy_serialized_dispatches() | changed
        })
}

fn upgrade_assignments(assignments: &mut [(super::AssignmentTargets, Expr)]) -> bool {
    assignments
        .iter_mut()
        .fold(false, |changed, (target, expression)| {
            upgrade_targets(target.targets_mut())
                | expression.upgrade_legacy_serialized_dispatches()
                | changed
        })
}

fn upgrade_ctes(ctes: &mut [CTE]) -> bool {
    ctes.iter_mut().fold(false, |mut changed, cte| {
        if let Some(cycle) = &mut cte.cycle {
            changed |= cycle.mark_value.upgrade_legacy_serialized_dispatches();
            changed |= cycle.mark_default.upgrade_legacy_serialized_dispatches();
        }
        let mut statement = cte.body.clone().into_statement();
        let body_changed = statement.upgrade_legacy_serialized_dispatches();
        if body_changed {
            cte.body = super::CteBody::try_from(statement)
                .expect("dispatch migration preserves the CTE statement kind");
        }
        changed | body_changed
    })
}

impl FromClause {
    fn upgrade_legacy_serialized_dispatches(&mut self) -> bool {
        match self {
            Self::Table { .. } => false,
            Self::Join {
                left, right, on, ..
            } => {
                left.upgrade_legacy_serialized_dispatches()
                    | right.upgrade_legacy_serialized_dispatches()
                    | upgrade_optional(on)
            }
            Self::Values { rows, .. } => upgrade_rows(rows),
            Self::Function { binding, args, .. } => {
                crate::semantics::upgrade_legacy_table_function_binding(binding, args.len())
                    | upgrade_exprs(args)
            }
            Self::FunctionGroup { functions, .. } => {
                functions.iter_mut().fold(false, |changed, function| {
                    crate::semantics::upgrade_legacy_table_function_binding(
                        &mut function.binding,
                        function.args.len(),
                    ) | upgrade_exprs(&mut function.args)
                        | changed
                })
            }
            Self::Subquery { body, .. } => body.upgrade_legacy_serialized_dispatches(),
        }
    }
}

impl SelectStmt {
    /// Upgrade every legacy compiler dispatch marker in this complete query tree.
    #[doc(hidden)]
    pub fn upgrade_legacy_serialized_dispatches(&mut self) -> bool {
        let mut changed = upgrade_projections(&mut self.projections);
        for window in &mut self.windows {
            for expression in window.spec.expressions_mut() {
                changed |= expression.upgrade_legacy_serialized_dispatches();
            }
        }
        changed |= upgrade_rows(&mut self.values);
        if let Some(from) = &mut self.from {
            changed |= from.upgrade_legacy_serialized_dispatches();
        }
        changed |= upgrade_optional(&mut self.r#where);
        changed |= upgrade_exprs(&mut self.group_by);
        for grouping_set in &mut self.grouping_sets {
            changed |= upgrade_exprs(grouping_set);
        }
        changed |= upgrade_optional(&mut self.having);
        for order in &mut self.order_by {
            changed |= order.expr.upgrade_legacy_serialized_dispatches();
        }
        changed |= upgrade_optional(&mut self.limit);
        changed |= upgrade_optional(&mut self.offset);
        changed |= upgrade_ctes(&mut self.with);
        if let Some(set_operation) = &mut self.set_op {
            if let Some(left) = &mut set_operation.left {
                changed |= left.upgrade_legacy_serialized_dispatches();
            }
            changed |= set_operation.right.upgrade_legacy_serialized_dispatches();
            for order in &mut set_operation.combined_order_by {
                changed |= order.expr.upgrade_legacy_serialized_dispatches();
            }
            changed |= upgrade_optional(&mut set_operation.combined_limit);
            changed |= upgrade_optional(&mut set_operation.combined_offset);
        }
        changed | upgrade_exprs(&mut self.distinct_on)
    }
}

impl MergeWhen {
    fn upgrade_legacy_serialized_dispatches(&mut self) -> bool {
        match self {
            Self::UpdateMatched {
                condition,
                assignments,
            }
            | Self::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => upgrade_optional(condition) | upgrade_assignments(assignments),
            Self::InsertNotMatched {
                condition,
                columns,
                values,
                ..
            } => upgrade_optional(condition) | upgrade_targets(columns) | upgrade_exprs(values),
            Self::DeleteMatched { condition }
            | Self::DeleteNotMatchedBySource { condition }
            | Self::NothingMatched { condition }
            | Self::NothingNotMatched { condition }
            | Self::NothingNotMatchedBySource { condition } => upgrade_optional(condition),
        }
    }
}

impl Statement {
    /// Upgrade legacy compiler dispatch markers without reparsing SQL or changing catalog-bound relation identities.
    #[doc(hidden)]
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive AST migration preserves every serialized variant"
    )]
    pub fn upgrade_legacy_serialized_dispatches(&mut self) -> bool {
        match self {
            Self::Select(select) => select.upgrade_legacy_serialized_dispatches(),
            Self::CreateEnum(_)
            | Self::CreateCompositeType(_)
            | Self::AlterEnum(_)
            | Self::AlterTypeObject(_)
            | Self::GrantType(_) => false,
            Self::CreateDomain(domain) => {
                let mut changed = upgrade_optional(&mut domain.default);
                for check in &mut domain.checks {
                    changed |= check.expression.upgrade_legacy_serialized_dispatches();
                }
                changed
            }
            Self::AlterDomain(domain) => match &mut domain.action {
                crate::ast::AlterDomainAction::AddCheck { constraint } => {
                    constraint.expression.upgrade_legacy_serialized_dispatches()
                }
                crate::ast::AlterDomainAction::AddNotNull { .. }
                | crate::ast::AlterDomainAction::DropConstraint { .. }
                | crate::ast::AlterDomainAction::ValidateConstraint { .. } => false,
            },
            Self::Insert(insert) => {
                let mut changed = upgrade_ctes(&mut insert.with);
                changed |= upgrade_targets(&mut insert.columns);
                changed |= upgrade_rows(&mut insert.rows);
                if let Some(source) = &mut insert.select_source {
                    changed |= source.upgrade_legacy_serialized_dispatches();
                }
                if let Some(conflict) = &mut insert.on_conflict {
                    for expression in &mut conflict.expressions {
                        changed |= expression.upgrade_legacy_serialized_dispatches();
                    }
                    changed |= conflict
                        .predicate
                        .as_deref_mut()
                        .is_some_and(Expr::upgrade_legacy_serialized_dispatches);
                    if let OnConflictAction::Update {
                        assignments,
                        r#where,
                    } = &mut conflict.action
                    {
                        changed |= upgrade_assignments(assignments);
                        changed |= r#where
                            .as_deref_mut()
                            .is_some_and(Expr::upgrade_legacy_serialized_dispatches);
                    }
                }
                changed | upgrade_projections(&mut insert.returning)
            }
            Self::Update(update) => {
                let mut changed = upgrade_assignments(&mut update.assignments);
                changed |= upgrade_optional(&mut update.r#where);
                changed |= upgrade_ctes(&mut update.with);
                if let Some(from) = &mut update.from {
                    changed |= from.upgrade_legacy_serialized_dispatches();
                }
                changed | upgrade_projections(&mut update.returning)
            }
            Self::Delete(delete) => {
                let mut changed = upgrade_optional(&mut delete.r#where);
                changed |= upgrade_ctes(&mut delete.with);
                if let Some(using) = &mut delete.using {
                    changed |= using.upgrade_legacy_serialized_dispatches();
                }
                changed | upgrade_projections(&mut delete.returning)
            }
            Self::CreateView { body, .. }
            | Self::CreateMaterializedView { body, .. }
            | Self::CreateTableAs { body, .. } => body.upgrade_legacy_serialized_dispatches(),
            Self::DeclareCursor(cursor) => cursor.query.upgrade_legacy_serialized_dispatches(),
            Self::Explain { body, .. } | Self::Prepare { body, .. } => {
                body.upgrade_legacy_serialized_dispatches()
            }
            Self::Execute { params, .. } | Self::Call { args: params, .. } => upgrade_exprs(params),
            Self::Values { rows } => upgrade_rows(rows),
            Self::Merge(merge) => {
                let mut changed = upgrade_ctes(&mut merge.with);
                changed |= merge.source.upgrade_legacy_serialized_dispatches();
                changed |= merge.join_condition.upgrade_legacy_serialized_dispatches();
                for clause in &mut merge.when_clauses {
                    changed |= clause.upgrade_legacy_serialized_dispatches();
                }
                changed | upgrade_projections(&mut merge.returning)
            }
            Self::CreateFunction(definition) => {
                let mut changed = definition
                    .params
                    .iter_mut()
                    .fold(false, |changed, parameter| {
                        parameter
                            .default
                            .as_mut()
                            .is_some_and(Expr::upgrade_legacy_serialized_dispatches)
                            | changed
                    });
                if let FunctionBody::Statements(statements) = &mut definition.body {
                    for statement in statements {
                        changed |= statement.upgrade_legacy_serialized_dispatches();
                    }
                }
                changed
            }
            Self::CreateTrigger(trigger) => upgrade_optional(&mut trigger.when),
            Self::CreateRule(rule) => {
                let mut changed = upgrade_optional(&mut rule.condition);
                for action in &mut rule.actions {
                    changed |= action.upgrade_legacy_serialized_dispatches();
                }
                changed
            }
            Self::CreateTable(_)
            | Self::CreateTableIfNotExists(_)
            | Self::CreateIndex(_)
            | Self::RenameIndex(_)
            | Self::Drop(_)
            | Self::AlterTable(_)
            | Self::AlterForeignTable(_)
            | Self::AlterView(_)
            | Self::RefreshMaterializedView { .. }
            | Self::CreateSchema { .. }
            | Self::AlterSchemaOwner { .. }
            | Self::RenameSchema { .. }
            | Self::Notify { .. }
            | Self::Listen { .. }
            | Self::Unlisten { .. }
            | Self::SetVariable { .. }
            | Self::ResetVariable { .. }
            | Self::ResetAllVariables
            | Self::SetConstraints { .. }
            | Self::ShowVariable { .. }
            | Self::Discard { .. }
            | Self::Load { .. }
            | Self::Analyze { .. }
            | Self::Vacuum(_)
            | Self::LockTable(_)
            | Self::Truncate { .. }
            | Self::Transaction(_)
            | Self::FetchCursor(_)
            | Self::CloseCursor { .. }
            | Self::CreateSequence(_)
            | Self::AlterSequence(_)
            | Self::Deallocate { .. }
            | Self::CreateForeignWrapper(_)
            | Self::CreateForeignServer(_)
            | Self::CreateForeignTable(_)
            | Self::CreateForeignTableDefinition(_)
            | Self::DropFunction(_)
            | Self::AlterRoutine(_)
            | Self::AlterRoutineOwner(_)
            | Self::RenameRoutine(_)
            | Self::GrantRoutine(_)
            | Self::GrantTable(_)
            | Self::GrantSequence(_)
            | Self::GrantDatabase(_)
            | Self::GrantSchema(_)
            | Self::GrantRole(_)
            | Self::CreateRole(_)
            | Self::AlterRole(_)
            | Self::RenameRole(_)
            | Self::DropRole(_)
            | Self::DropTrigger(_)
            | Self::DropRule(_)
            | Self::DoBlock { .. } => false,
        }
    }
}
