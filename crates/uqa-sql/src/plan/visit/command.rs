//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow command expressions directly, including plans retained by utility commands.

use super::{visit_ctes, CommandPlan, QueryPlan, ScalarExpr};
use crate::plan::{AssignmentPlan, ConflictActionPlan, MergeWhenPlan, ProjectionPlan};

fn assignments(values: &[AssignmentPlan], visit: &mut dyn FnMut(&ScalarExpr)) {
    for expression in values.iter().flat_map(AssignmentPlan::expressions) {
        visit(expression);
    }
}

fn projections(values: &[ProjectionPlan], visit: &mut dyn FnMut(&ScalarExpr)) {
    for projection in values {
        visit(&projection.expr);
    }
}

fn subqueries(values: &[QueryPlan], visit: &mut dyn FnMut(&ScalarExpr)) {
    for query in values {
        query.visit_scalar_expressions(visit);
    }
}

impl CommandPlan {
    /// Visit scalar roots without copying the plan or collecting its children. Mutations retain their analysis order: CTEs, direct expressions, query inputs, then source inputs. Query-bearing utilities share the scalar rewrite's ownership boundary.
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive command ownership without temporary child collections"
    )]
    pub fn visit_scalar_expressions(&self, visit: &mut dyn FnMut(&ScalarExpr)) {
        match self {
            Self::Insert(plan) => {
                visit_ctes(&plan.ctes, visit);
                for expression in plan
                    .columns
                    .iter()
                    .flat_map(crate::ast::AssignmentTarget::expressions)
                {
                    visit(expression);
                }
                for expression in plan.rows.iter().flatten() {
                    visit(expression);
                }
                if let Some(conflict) = &plan.on_conflict {
                    for expression in &conflict.expressions {
                        visit(expression);
                    }
                    if let Some(predicate) = &conflict.predicate {
                        visit(predicate);
                    }
                    if let ConflictActionPlan::Update {
                        assignments: values,
                        predicate,
                    } = &conflict.action
                    {
                        assignments(values, visit);
                        if let Some(predicate) = predicate {
                            visit(predicate);
                        }
                    }
                }
                projections(&plan.returning, visit);
                for check in &plan.view_checks {
                    visit(&check.predicate);
                }
                if let Some(source) = &plan.source {
                    source.visit_scalar_expressions(visit);
                }
                subqueries(&plan.subqueries, visit);
            }
            Self::Update(plan) => {
                visit_ctes(&plan.ctes, visit);
                assignments(&plan.assignments, visit);
                if let Some(predicate) = &plan.predicate {
                    visit(predicate);
                }
                projections(&plan.returning, visit);
                for check in &plan.view_checks {
                    visit(&check.predicate);
                }
                subqueries(&plan.subqueries, visit);
                if let Some(source) = &plan.source {
                    source.visit_scalar_expressions(visit);
                }
            }
            Self::Delete(plan) => {
                visit_ctes(&plan.ctes, visit);
                if let Some(predicate) = &plan.predicate {
                    visit(predicate);
                }
                projections(&plan.returning, visit);
                subqueries(&plan.subqueries, visit);
                if let Some(source) = &plan.source {
                    source.visit_scalar_expressions(visit);
                }
            }
            Self::Merge(plan) => {
                visit_ctes(&plan.ctes, visit);
                if let Some(predicate) = &plan.target_predicate {
                    visit(predicate);
                }
                visit(&plan.join_condition);
                for clause in &plan.when_clauses {
                    match clause {
                        MergeWhenPlan::UpdateMatched {
                            condition,
                            assignments: values,
                        }
                        | MergeWhenPlan::UpdateNotMatchedBySource {
                            condition,
                            assignments: values,
                        } => {
                            if let Some(condition) = condition {
                                visit(condition);
                            }
                            assignments(values, visit);
                        }
                        MergeWhenPlan::InsertNotMatched {
                            condition,
                            columns,
                            values,
                            ..
                        } => {
                            if let Some(condition) = condition {
                                visit(condition);
                            }
                            for expression in columns
                                .iter()
                                .flat_map(crate::ast::AssignmentTarget::expressions)
                            {
                                visit(expression);
                            }
                            for expression in values {
                                visit(expression);
                            }
                        }
                        MergeWhenPlan::DeleteMatched { condition }
                        | MergeWhenPlan::DeleteNotMatchedBySource { condition }
                        | MergeWhenPlan::NothingMatched { condition }
                        | MergeWhenPlan::NothingNotMatched { condition }
                        | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                            if let Some(condition) = condition {
                                visit(condition);
                            }
                        }
                    }
                }
                projections(&plan.returning, visit);
                for check in &plan.view_checks {
                    visit(&check.predicate);
                }
                subqueries(&plan.subqueries, visit);
                plan.source.visit_scalar_expressions(visit);
            }
            Self::CreateView { query, .. }
            | Self::CreateMaterializedView { query, .. }
            | Self::CreateTableAs { query, .. }
            | Self::DeclareCursor { query, .. } => query.visit_scalar_expressions(visit),
            Self::Explain { body, .. } | Self::Prepare { body, .. } => {
                body.visit_scalar_expressions(visit);
            }
            Self::Execute { params, .. } | Self::Call { args: params, .. } => {
                for expression in params {
                    visit(&expression.scalar);
                    subqueries(&expression.subqueries, visit);
                }
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
            | Self::CreateDomain(_)
            | Self::AlterDomain(_)
            | Self::CommentType(_)
            | Self::CreateShellType(_)
            | Self::CreateEnum(_)
            | Self::CreateCompositeType(_)
            | Self::AlterEnum(_)
            | Self::AlterTypeObject(_)
            | Self::GrantType(_)
            | Self::AlterSequence(_)
            | Self::Deallocate { .. }
            | Self::CreateForeignWrapper(_)
            | Self::CreateForeignServer(_)
            | Self::CreateForeignTable(_)
            | Self::CreateForeignTableDefinition(_)
            | Self::CreateFunction(_)
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
            | Self::CreateTrigger(_)
            | Self::DropTrigger(_)
            | Self::CreateRule(_)
            | Self::DropRule(_)
            | Self::DoBlock { .. }
            | Self::Empty => {}
        }
    }
}
