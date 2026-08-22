//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Top-level SQL statement lowering and command naming.

use super::model::NoRegisteredAggregates;
use super::query::{lower_assignments, lower_ctes, lower_merge_when};
use super::rewrite::{rewrite_command_scalars, rewrite_query_scalars};
use super::scalar::lower_scalar_expression;
use super::{
    AggregateClassifier, CommandPlan, ConflictActionPlan, ConflictPlan, DeletePlan, ExpressionPlan,
    InsertPlan, MergePlan, ProjectionPlan, QueryPlan, RelationalPlan, ScalarExpr, SourcePlan,
    Statement, UnifiedPlan, UpdatePlan,
};

impl UnifiedPlan {
    /// Lower a statement using only the built-in SQL aggregate catalogue.
    #[must_use]
    pub fn lower(statement: Statement) -> Self {
        Self::lower_with(statement, &NoRegisteredAggregates)
    }

    /// Lower a statement with engine-local aggregate classification.
    #[must_use]
    pub fn lower_with(statement: Statement, aggregates: &dyn AggregateClassifier) -> Self {
        match statement {
            Statement::Select(query) => {
                Self::Query(Box::new(QueryPlan::lower_with(*query, aggregates)))
            }
            Statement::Values { rows } => {
                let mut subqueries = Vec::new();
                let rows = rows
                    .into_iter()
                    .map(|row| {
                        row.into_iter()
                            .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries))
                            .collect()
                    })
                    .collect();
                Self::Query(Box::new(QueryPlan {
                    ctes: Vec::new(),
                    root: RelationalPlan::Values { rows, subqueries },
                }))
            }
            Statement::CreateTable(value) => {
                Self::Command(Box::new(CommandPlan::CreateTable(value)))
            }
            Statement::CreateIndex(value) => {
                Self::Command(Box::new(CommandPlan::CreateIndex(value)))
            }
            Statement::Insert(statement) => {
                let ctes = lower_ctes(&statement.with, aggregates);
                let source = statement
                    .select_source
                    .map(|query| Box::new(QueryPlan::lower_with(*query, aggregates)));
                let mut subqueries = Vec::new();
                let rows = statement
                    .rows
                    .into_iter()
                    .map(|row| {
                        row.into_iter()
                            .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries))
                            .collect()
                    })
                    .collect();
                let on_conflict = statement.on_conflict.map(|conflict| {
                    let action = match conflict.action {
                        uqa_sql::ast::OnConflictAction::Nothing => ConflictActionPlan::Nothing,
                        uqa_sql::ast::OnConflictAction::Update {
                            assignments,
                            r#where,
                        } => ConflictActionPlan::Update {
                            assignments: lower_assignments(
                                assignments,
                                aggregates,
                                &mut subqueries,
                            ),
                            predicate: r#where.map(|expr| {
                                lower_scalar_expression(expr, aggregates, &mut subqueries)
                            }),
                        },
                    };
                    ConflictPlan {
                        conflict_columns: conflict.conflict_columns,
                        action,
                    }
                });
                let returning = statement
                    .returning
                    .into_iter()
                    .map(|projection| {
                        ProjectionPlan::lower_with(projection, aggregates, &mut subqueries)
                    })
                    .collect();
                Self::Command(Box::new(CommandPlan::Insert(Box::new(InsertPlan {
                    table: statement.table,
                    target_qualifier: statement.target_qualifier,
                    columns: statement.columns,
                    ctes,
                    rows,
                    source,
                    on_conflict,
                    returning,
                    returning_aliases: statement.returning_aliases,
                    subqueries,
                }))))
            }
            Statement::Update(statement) => {
                let ctes = lower_ctes(&statement.with, aggregates);
                let mut subqueries = Vec::new();
                let source = statement
                    .from
                    .map(|from| SourcePlan::lower_with(from, aggregates, &mut subqueries));
                let assignments =
                    lower_assignments(statement.assignments, aggregates, &mut subqueries);
                let predicate = statement
                    .r#where
                    .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries));
                let returning = statement
                    .returning
                    .into_iter()
                    .map(|projection| {
                        ProjectionPlan::lower_with(projection, aggregates, &mut subqueries)
                    })
                    .collect();
                Self::Command(Box::new(CommandPlan::Update(Box::new(UpdatePlan {
                    table: statement.table,
                    target_qualifier: statement.target_qualifier,
                    assignments,
                    predicate,
                    ctes,
                    source: source.map(Box::new),
                    returning,
                    returning_aliases: statement.returning_aliases,
                    subqueries,
                }))))
            }
            Statement::Delete(statement) => {
                let ctes = lower_ctes(&statement.with, aggregates);
                let mut subqueries = Vec::new();
                let source = statement
                    .using
                    .map(|from| SourcePlan::lower_with(from, aggregates, &mut subqueries));
                let predicate = statement
                    .r#where
                    .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries));
                let returning = statement
                    .returning
                    .into_iter()
                    .map(|projection| {
                        ProjectionPlan::lower_with(projection, aggregates, &mut subqueries)
                    })
                    .collect();
                Self::Command(Box::new(CommandPlan::Delete(Box::new(DeletePlan {
                    table: statement.table,
                    target_qualifier: statement.target_qualifier,
                    predicate,
                    ctes,
                    source: source.map(Box::new),
                    returning,
                    returning_aliases: statement.returning_aliases,
                    subqueries,
                }))))
            }
            Statement::Drop(value) => Self::Command(Box::new(CommandPlan::Drop(value))),
            Statement::AlterTable(value) => {
                Self::Command(Box::new(CommandPlan::AlterTable(Box::new(value))))
            }
            Statement::CreateView {
                name,
                body,
                or_replace,
            } => {
                let query = Box::new(QueryPlan::lower_with(*body, aggregates));
                Self::Command(Box::new(CommandPlan::CreateView {
                    name,
                    query,
                    or_replace,
                }))
            }
            Statement::CreateSchema {
                name,
                if_not_exists,
            } => Self::Command(Box::new(CommandPlan::CreateSchema {
                name,
                if_not_exists,
            })),
            Statement::SetVariable { name, value } => {
                Self::Command(Box::new(CommandPlan::SetVariable { name, value }))
            }
            Statement::ShowVariable { name } => {
                Self::Command(Box::new(CommandPlan::ShowVariable { name }))
            }
            Statement::Discard { target } => {
                Self::Command(Box::new(CommandPlan::Discard { target }))
            }
            Statement::Load { library } => Self::Command(Box::new(CommandPlan::Load { library })),
            Statement::Explain {
                analyze,
                verbose,
                format,
                body,
            } => Self::Command(Box::new(CommandPlan::Explain {
                analyze,
                verbose,
                format,
                body: Box::new(Self::lower_with(*body, aggregates)),
            })),
            Statement::Analyze { table } => Self::Command(Box::new(CommandPlan::Analyze { table })),
            Statement::Truncate { tables, cascade } => {
                Self::Command(Box::new(CommandPlan::Truncate { tables, cascade }))
            }
            Statement::Transaction(value) => {
                Self::Command(Box::new(CommandPlan::Transaction(value)))
            }
            Statement::CreateSequence(value) => {
                Self::Command(Box::new(CommandPlan::CreateSequence(value)))
            }
            Statement::AlterSequence(value) => {
                Self::Command(Box::new(CommandPlan::AlterSequence(value)))
            }
            Statement::CreateTableAs {
                name,
                if_not_exists,
                column_names,
                with_no_data,
                body,
            } => Self::Command(Box::new(CommandPlan::CreateTableAs {
                name,
                if_not_exists,
                column_names,
                with_no_data,
                query: Box::new(QueryPlan::lower_with(*body, aggregates)),
            })),
            Statement::Prepare { name, body } => {
                let body = Box::new(Self::lower_with(*body, aggregates));
                Self::Command(Box::new(CommandPlan::Prepare { name, body }))
            }
            Statement::Execute { name, params } => Self::Command(Box::new(CommandPlan::Execute {
                name,
                params: params
                    .into_iter()
                    .map(|expr| ExpressionPlan::lower_with(expr, aggregates))
                    .collect(),
            })),
            Statement::Deallocate { name } => {
                Self::Command(Box::new(CommandPlan::Deallocate { name }))
            }
            Statement::CreateForeignServer(value) => {
                Self::Command(Box::new(CommandPlan::CreateForeignServer(value)))
            }
            Statement::CreateForeignTable(value) => {
                Self::Command(Box::new(CommandPlan::CreateForeignTable(value)))
            }
            Statement::Merge(statement) => {
                let mut subqueries = Vec::new();
                let source = SourcePlan::lower_with(statement.source, aggregates, &mut subqueries);
                let join_condition =
                    lower_scalar_expression(statement.join_condition, aggregates, &mut subqueries);
                let when_clauses = statement
                    .when_clauses
                    .into_iter()
                    .map(|clause| lower_merge_when(clause, aggregates, &mut subqueries))
                    .collect();
                let returning = statement
                    .returning
                    .into_iter()
                    .map(|projection| {
                        ProjectionPlan::lower_with(projection, aggregates, &mut subqueries)
                    })
                    .collect();
                Self::Command(Box::new(CommandPlan::Merge(Box::new(MergePlan {
                    target: statement.target,
                    target_qualifier: statement.target_qualifier,
                    target_alias: statement.target_alias,
                    source: Box::new(source),
                    join_condition,
                    when_clauses,
                    returning,
                    returning_aliases: statement.returning_aliases,
                    subqueries,
                }))))
            }
            Statement::CreateFunction(value) => {
                Self::Command(Box::new(CommandPlan::CreateFunction(value)))
            }
            Statement::DropFunction(value) => {
                Self::Command(Box::new(CommandPlan::DropFunction(value)))
            }
            Statement::DoBlock { language, body } => {
                Self::Command(Box::new(CommandPlan::DoBlock { language, body }))
            }
            Statement::Call { name, args } => Self::Command(Box::new(CommandPlan::Call {
                name,
                args: args
                    .into_iter()
                    .map(|expr| ExpressionPlan::lower_with(expr, aggregates))
                    .collect(),
            })),
        }
    }

    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Query(_) => "Query",
            Self::Command(command) => command.name(),
        }
    }

    /// Rewrite every physical scalar slot owned by this plan, including
    /// CTEs, scalar subqueries, mutation sources, prepared/explained bodies,
    /// and routine-call arguments.  This is the plan-native binding hook used
    /// by SQL-language routines; callers never need to reconstruct an AST to
    /// specialize a stored plan.
    pub fn rewrite_scalar_expressions(&mut self, rewrite: &mut dyn FnMut(&mut ScalarExpr)) {
        match self {
            Self::Query(query) => rewrite_query_scalars(query, rewrite),
            Self::Command(command) => rewrite_command_scalars(command, rewrite),
        }
    }
}

impl CommandPlan {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::CreateTable(_) => "CreateTable",
            Self::CreateIndex(_) => "CreateIndex",
            Self::Insert(_) => "Insert",
            Self::Update(_) => "Update",
            Self::Delete(_) => "Delete",
            Self::Drop(_) => "Drop",
            Self::AlterTable(_) => "AlterTable",
            Self::CreateView { .. } => "CreateView",
            Self::CreateSchema { .. } => "CreateSchema",
            Self::SetVariable { .. } => "SetVariable",
            Self::ShowVariable { .. } => "ShowVariable",
            Self::Discard { .. } => "Discard",
            Self::Load { .. } => "Load",
            Self::Explain { .. } => "Explain",
            Self::Analyze { .. } => "Analyze",
            Self::Truncate { .. } => "Truncate",
            Self::Transaction(_) => "Transaction",
            Self::CreateSequence(_) => "CreateSequence",
            Self::AlterSequence(_) => "AlterSequence",
            Self::CreateTableAs { .. } => "CreateTableAs",
            Self::Prepare { .. } => "Prepare",
            Self::Execute { .. } => "Execute",
            Self::Deallocate { .. } => "Deallocate",
            Self::CreateForeignServer(_) => "CreateForeignServer",
            Self::CreateForeignTable(_) => "CreateForeignTable",
            Self::Merge(_) => "Merge",
            Self::CreateFunction(_) => "CreateFunction",
            Self::DropFunction(_) => "DropFunction",
            Self::DoBlock { .. } => "DoBlock",
            Self::Call { .. } => "Call",
        }
    }
}
