//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete SQL-to-execution-plan lowering.
//!
//! [`OperatorTree`](uqa_operators::OperatorTree) is the specialised algebra
//! for posting-list, graph, and fusion operations.  It is intentionally not a
//! relational algebra: forcing a SQL window frame or a mutation into a
//! posting-list node would erase its row and command semantics.  This module
//! supplies the missing super-plan.  Every SQL statement lowers to one
//! [`UnifiedPlan`], while query-producing statements recursively own their
//! relational children.  A physical driver can therefore use an
//! `OperatorTree` as an access path *inside* a relational node without keeping
//! a second top-level SQL dispatcher.

use uqa_execution::{
    ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec,
};
use uqa_sql::ast::{
    Expr, FrameBound, FromClause, NullsOrder, OrderBy, Projection, SelectStmt, SetOpKind,
    Statement, WindowSpec, CTE,
};

/// One fully lowered SQL statement.
///
/// There is deliberately no `Legacy`, `Opaque`, or raw-`Statement` variant:
/// adding a SQL statement kind must update the exhaustive lowerer and the
/// physical driver.
#[derive(Debug, Clone)]
pub enum UnifiedPlan {
    Query(Box<QueryPlan>),
    Command(Box<CommandPlan>),
}

/// A relational query with its CTE scope and one relational root.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueryPlan {
    pub ctes: Vec<CtePlan>,
    pub root: RelationalPlan,
}

/// A named query child owned by a [`QueryPlan`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CtePlan {
    pub name: String,
    pub columns: Vec<String>,
    pub recursive: bool,
    pub query: Box<QueryPlan>,
}

/// Relational nodes common to ordinary SQL, retrieval SQL, and table/graph
/// functions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RelationalPlan {
    /// A single SELECT query block. Its source is a separate plan tree and its
    /// compute phase is classified as projection, aggregation, or windowing.
    QueryBlock(Box<QueryBlockPlan>),
    /// SQL set operations own both input plans; combined ordering and slicing
    /// are properties of the set node rather than either branch.
    SetOp {
        kind: SetOpKind,
        all: bool,
        left: Box<QueryPlan>,
        right: Box<QueryPlan>,
        order_by: Vec<OrderPlan>,
        limit: Option<Box<ScalarExpr>>,
        offset: Option<Box<ScalarExpr>>,
        subqueries: Vec<QueryPlan>,
    },
    /// Standalone `VALUES`, used both as a statement and as a relational
    /// source. Each cell remains an expression so parameters/functions bind at
    /// execution time.
    Values {
        rows: Vec<Vec<ScalarExpr>>,
        subqueries: Vec<QueryPlan>,
    },
}

/// One SELECT block after `WITH` and set-operation structure has been pulled
/// into explicit parent/child nodes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueryBlockPlan {
    pub projections: Vec<ProjectionPlan>,
    pub from: Option<SourcePlan>,
    pub r#where: Option<ScalarExpr>,
    pub compute: ComputePlan,
    pub group_by: Vec<ScalarExpr>,
    pub grouping_sets: Vec<Vec<ScalarExpr>>,
    pub having: Option<ScalarExpr>,
    pub order_by: Vec<OrderPlan>,
    pub limit: Option<ScalarExpr>,
    pub offset: Option<ScalarExpr>,
    pub distinct: bool,
    pub distinct_on: Vec<ScalarExpr>,
    pub subqueries: Vec<QueryPlan>,
    pub access: AccessPathPlan,
}

/// Cross-paradigm access decision made after the relational and scalar
/// portions of a query block have both been lowered.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AccessPathPlan {
    /// Ordinary row-source execution.
    Row,
    /// Use the shared posting-list/operator algebra for the block predicate.
    OperatorTree {
        /// The relational ORDER BY/OFFSET/LIMIT can be pushed into the
        /// retrieval function before row materialization.
        score_limit_pushdown: bool,
    },
    /// Split a mixed predicate into posting-list candidates followed by
    /// row-level residual evaluation.
    Hybrid,
}

/// The row-producing source below a query block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum SourcePlan {
    Table {
        name: String,
        alias: Option<String>,
    },
    Join {
        left: Box<SourcePlan>,
        right: Box<SourcePlan>,
        kind: uqa_sql::ast::JoinKind,
        on: Option<ScalarExpr>,
        lateral: bool,
    },
    Values {
        rows: Vec<Vec<ScalarExpr>>,
        alias: Option<String>,
        column_aliases: Vec<String>,
    },
    Function {
        name: String,
        args: Vec<ScalarExpr>,
        alias: Option<String>,
        column_aliases: Vec<String>,
        column_types: Vec<String>,
    },
    Subquery {
        body: Box<QueryPlan>,
        alias: Option<String>,
        column_aliases: Vec<String>,
    },
}

/// The SELECT-list phase chosen during lowering.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ComputePlan {
    Project,
    Aggregate,
    Window,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectionPlan {
    pub expr: ScalarExpr,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OrderPlan {
    pub expr: ScalarExpr,
    pub descending: bool,
    pub nulls: Option<NullsOrder>,
}

/// Executable scalar IR plus every query-valued descendant it owns.
#[derive(Debug, Clone)]
pub struct ExpressionPlan {
    pub scalar: ScalarExpr,
    pub subqueries: Vec<QueryPlan>,
}

#[derive(Debug, Clone)]
pub struct AssignmentPlan {
    pub column: String,
    pub value: ScalarExpr,
}

#[derive(Debug, Clone)]
pub struct InsertPlan {
    pub table: String,
    pub columns: Vec<String>,
    pub ctes: Vec<CtePlan>,
    pub rows: Vec<Vec<ScalarExpr>>,
    pub source: Option<Box<QueryPlan>>,
    pub on_conflict: Option<ConflictPlan>,
    pub returning: Vec<ProjectionPlan>,
    pub subqueries: Vec<QueryPlan>,
}

#[derive(Debug, Clone)]
pub struct ConflictPlan {
    pub conflict_columns: Vec<String>,
    pub action: ConflictActionPlan,
}

#[derive(Debug, Clone)]
pub enum ConflictActionPlan {
    Nothing,
    Update {
        assignments: Vec<AssignmentPlan>,
        predicate: Option<ScalarExpr>,
    },
}

#[derive(Debug, Clone)]
pub struct UpdatePlan {
    pub table: String,
    pub assignments: Vec<AssignmentPlan>,
    pub predicate: Option<ScalarExpr>,
    pub ctes: Vec<CtePlan>,
    pub source: Option<Box<SourcePlan>>,
    pub returning: Vec<ProjectionPlan>,
    pub subqueries: Vec<QueryPlan>,
}

#[derive(Debug, Clone)]
pub struct DeletePlan {
    pub table: String,
    pub predicate: Option<ScalarExpr>,
    pub ctes: Vec<CtePlan>,
    pub source: Option<Box<SourcePlan>>,
    pub returning: Vec<ProjectionPlan>,
    pub subqueries: Vec<QueryPlan>,
}

#[derive(Debug, Clone)]
pub struct MergePlan {
    pub target: String,
    pub target_alias: Option<String>,
    pub source: Box<SourcePlan>,
    pub join_condition: ScalarExpr,
    pub when_clauses: Vec<MergeWhenPlan>,
    pub returning: Vec<ProjectionPlan>,
    pub subqueries: Vec<QueryPlan>,
}

#[derive(Debug, Clone)]
pub enum MergeWhenPlan {
    UpdateMatched {
        condition: Option<ScalarExpr>,
        assignments: Vec<AssignmentPlan>,
    },
    DeleteMatched {
        condition: Option<ScalarExpr>,
    },
    InsertNotMatched {
        condition: Option<ScalarExpr>,
        columns: Vec<String>,
        values: Vec<ScalarExpr>,
    },
    NothingMatched {
        condition: Option<ScalarExpr>,
    },
    NothingNotMatched {
        condition: Option<ScalarExpr>,
    },
}

/// Non-query statement plans. Mutations own physical sources and scalar IR;
/// query-bearing catalog commands own explicit query children. Typed DDL and
/// procedural payloads contain catalog data, never a second SQL dispatcher.
#[derive(Debug, Clone)]
pub enum CommandPlan {
    CreateTable(uqa_sql::ast::CreateTable),
    CreateIndex(uqa_sql::ast::CreateIndex),
    Insert(Box<InsertPlan>),
    Update(Box<UpdatePlan>),
    Delete(Box<DeletePlan>),
    Drop(uqa_sql::ast::DropStmt),
    AlterTable(Box<uqa_sql::ast::AlterTableStmt>),
    CreateView {
        name: String,
        query: Box<QueryPlan>,
        or_replace: bool,
    },
    CreateSchema {
        name: String,
        if_not_exists: bool,
    },
    SetVariable {
        name: String,
        value: String,
    },
    ShowVariable {
        name: String,
    },
    Discard {
        target: uqa_sql::ast::DiscardTarget,
    },
    Explain {
        analyze: bool,
        verbose: bool,
        format: Option<String>,
        body: Box<UnifiedPlan>,
    },
    Analyze {
        table: Option<String>,
    },
    Truncate {
        tables: Vec<String>,
        cascade: bool,
    },
    Transaction(uqa_sql::ast::TransactionStmt),
    CreateSequence(uqa_sql::ast::CreateSequence),
    AlterSequence(uqa_sql::ast::AlterSequence),
    CreateTableAs {
        name: String,
        if_not_exists: bool,
        query: Box<QueryPlan>,
    },
    Prepare {
        name: String,
        body: Box<UnifiedPlan>,
    },
    Execute {
        name: String,
        params: Vec<ExpressionPlan>,
    },
    Deallocate {
        name: Option<String>,
    },
    CreateForeignServer(uqa_sql::ast::CreateForeignServer),
    CreateForeignTable(uqa_sql::ast::CreateForeignTable),
    Merge(Box<MergePlan>),
    CreateFunction(Box<uqa_sql::ast::CreateFunction>),
    DropFunction(uqa_sql::ast::DropFunctionStmt),
    DoBlock {
        language: String,
        body: String,
    },
    Call {
        name: String,
        args: Vec<ExpressionPlan>,
    },
}

/// Classification hook for engine-registered aggregate functions. Built-in
/// aggregates are always recognised; the callback extends that set without
/// making the planner depend on the engine.
pub trait AggregateClassifier {
    fn is_registered_aggregate(&self, name: &str) -> bool;
}

impl<F> AggregateClassifier for F
where
    F: Fn(&str) -> bool,
{
    fn is_registered_aggregate(&self, name: &str) -> bool {
        self(name)
    }
}

struct NoRegisteredAggregates;

impl AggregateClassifier for NoRegisteredAggregates {
    fn is_registered_aggregate(&self, _name: &str) -> bool {
        false
    }
}

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
                    columns: statement.columns,
                    ctes,
                    rows,
                    source,
                    on_conflict,
                    returning,
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
                    assignments,
                    predicate,
                    ctes,
                    source: source.map(Box::new),
                    returning,
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
                    predicate,
                    ctes,
                    source: source.map(Box::new),
                    returning,
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
                body,
            } => Self::Command(Box::new(CommandPlan::CreateTableAs {
                name,
                if_not_exists,
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
                    target_alias: statement.target_alias,
                    source: Box::new(source),
                    join_condition,
                    when_clauses,
                    returning,
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
}

impl QueryPlan {
    #[must_use]
    pub fn lower(statement: SelectStmt) -> Self {
        Self::lower_with(statement, &NoRegisteredAggregates)
    }

    #[must_use]
    pub fn lower_with(mut statement: SelectStmt, aggregates: &dyn AggregateClassifier) -> Self {
        let ctes = lower_ctes(&statement.with, aggregates);
        statement.with.clear();
        let root = lower_relational_root(statement, aggregates);
        Self { ctes, root }
    }
}

fn lower_ctes(ctes: &[CTE], aggregates: &dyn AggregateClassifier) -> Vec<CtePlan> {
    ctes.iter()
        .map(|cte| CtePlan {
            name: cte.name.clone(),
            columns: cte.columns.clone(),
            recursive: cte.recursive,
            query: Box::new(QueryPlan::lower_with((*cte.query).clone(), aggregates)),
        })
        .collect()
}

fn lower_assignments(
    assignments: Vec<(String, Expr)>,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> Vec<AssignmentPlan> {
    assignments
        .into_iter()
        .map(|(column, expression)| AssignmentPlan {
            column,
            value: lower_scalar_expression(expression, aggregates, subqueries),
        })
        .collect()
}

fn lower_merge_when(
    clause: uqa_sql::ast::MergeWhen,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> MergeWhenPlan {
    let mut lower_optional = |expression: Option<Expr>| {
        expression.map(|expression| lower_scalar_expression(expression, aggregates, subqueries))
    };
    match clause {
        uqa_sql::ast::MergeWhen::UpdateMatched {
            condition,
            assignments,
        } => {
            let condition = lower_optional(condition);
            let assignments = lower_assignments(assignments, aggregates, subqueries);
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            }
        }
        uqa_sql::ast::MergeWhen::DeleteMatched { condition } => MergeWhenPlan::DeleteMatched {
            condition: lower_optional(condition),
        },
        uqa_sql::ast::MergeWhen::InsertNotMatched {
            condition,
            columns,
            values,
        } => {
            let condition = lower_optional(condition);
            let values = values
                .into_iter()
                .map(|value| lower_scalar_expression(value, aggregates, subqueries))
                .collect();
            MergeWhenPlan::InsertNotMatched {
                condition,
                columns,
                values,
            }
        }
        uqa_sql::ast::MergeWhen::NothingMatched { condition } => MergeWhenPlan::NothingMatched {
            condition: lower_optional(condition),
        },
        uqa_sql::ast::MergeWhen::NothingNotMatched { condition } => {
            MergeWhenPlan::NothingNotMatched {
                condition: lower_optional(condition),
            }
        }
    }
}
fn lower_relational_root(
    mut statement: SelectStmt,
    aggregates: &dyn AggregateClassifier,
) -> RelationalPlan {
    let Some(set_op) = statement.set_op.take() else {
        return RelationalPlan::QueryBlock(Box::new(QueryBlockPlan::lower_with(
            statement, aggregates,
        )));
    };

    let left = if let Some(left) = set_op.left {
        QueryPlan::lower_with(*left, aggregates)
    } else {
        QueryPlan {
            ctes: Vec::new(),
            root: RelationalPlan::QueryBlock(Box::new(QueryBlockPlan::lower_with(
                statement, aggregates,
            ))),
        }
    };
    let right = QueryPlan::lower_with(set_op.right, aggregates);
    let mut subqueries = Vec::new();
    RelationalPlan::SetOp {
        kind: set_op.kind,
        all: set_op.all,
        left: Box::new(left),
        right: Box::new(right),
        order_by: set_op
            .combined_order_by
            .into_iter()
            .map(|order| OrderPlan::lower_with(order, aggregates, &mut subqueries))
            .collect(),
        limit: set_op
            .combined_limit
            .map(|expr| Box::new(lower_scalar_expression(expr, aggregates, &mut subqueries))),
        offset: set_op
            .combined_offset
            .map(|expr| Box::new(lower_scalar_expression(expr, aggregates, &mut subqueries))),
        subqueries,
    }
}

impl QueryBlockPlan {
    fn lower_with(statement: SelectStmt, aggregates: &dyn AggregateClassifier) -> Self {
        debug_assert!(statement.with.is_empty());
        debug_assert!(statement.set_op.is_none());
        let mut subqueries = Vec::new();
        let projections: Vec<ProjectionPlan> = statement
            .projections
            .into_iter()
            .map(|projection| ProjectionPlan::lower_with(projection, aggregates, &mut subqueries))
            .collect();
        let is_aggregate =
            |name: &str| is_builtin_aggregate(name) || aggregates.is_registered_aggregate(name);
        let has_aggregate = !statement.group_by.is_empty()
            || !statement.grouping_sets.is_empty()
            || statement.having.is_some()
            || projections
                .iter()
                .any(|projection| projection.expr.contains_aggregate(&is_aggregate));
        let has_window = projections
            .iter()
            .any(|projection| projection.expr.contains_window());
        let compute = if has_aggregate {
            ComputePlan::Aggregate
        } else if has_window {
            ComputePlan::Window
        } else {
            ComputePlan::Project
        };
        Self {
            projections,
            from: statement
                .from
                .map(|source| SourcePlan::lower_with(source, aggregates, &mut subqueries)),
            r#where: statement
                .r#where
                .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries)),
            compute,
            group_by: statement
                .group_by
                .into_iter()
                .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries))
                .collect(),
            grouping_sets: statement
                .grouping_sets
                .into_iter()
                .map(|set| {
                    set.into_iter()
                        .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries))
                        .collect()
                })
                .collect(),
            having: statement
                .having
                .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries)),
            order_by: statement
                .order_by
                .into_iter()
                .map(|order| OrderPlan::lower_with(order, aggregates, &mut subqueries))
                .collect(),
            limit: statement
                .limit
                .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries)),
            offset: statement
                .offset
                .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries)),
            distinct: statement.distinct,
            distinct_on: statement
                .distinct_on
                .into_iter()
                .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries))
                .collect(),
            subqueries,
            access: AccessPathPlan::Row,
        }
    }

    /// Expression nodes evaluated while executing this query block. Query
    /// bodies under `FROM (SELECT ...)` are excluded because their child plan
    /// installs its own expression scope when it executes.
    #[must_use]
    pub fn expressions(&self) -> Vec<&ScalarExpr> {
        let mut expressions = Vec::new();
        if let Some(source) = &self.from {
            source.push_expressions(&mut expressions);
        }
        if let Some(filter) = &self.r#where {
            expressions.push(filter);
        }
        for projection in &self.projections {
            expressions.push(&projection.expr);
        }
        expressions.extend(&self.group_by);
        for set in &self.grouping_sets {
            expressions.extend(set);
        }
        if let Some(having) = &self.having {
            expressions.push(having);
        }
        expressions.extend(self.order_by.iter().map(|order| &order.expr));
        if let Some(limit) = &self.limit {
            expressions.push(limit);
        }
        if let Some(offset) = &self.offset {
            expressions.push(offset);
        }
        expressions.extend(&self.distinct_on);
        expressions
    }
}

impl SourcePlan {
    fn lower_with(
        source: FromClause,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Self {
        match source {
            FromClause::Table { name, alias } => Self::Table { name, alias },
            FromClause::Join {
                left,
                right,
                kind,
                on,
                lateral,
            } => Self::Join {
                left: Box::new(Self::lower_with(*left, aggregates, subqueries)),
                right: Box::new(Self::lower_with(*right, aggregates, subqueries)),
                kind,
                on: on.map(|expr| lower_scalar_expression(expr, aggregates, subqueries)),
                lateral,
            },
            FromClause::Values {
                rows,
                alias,
                column_aliases,
            } => Self::Values {
                rows: rows
                    .into_iter()
                    .map(|row| {
                        row.into_iter()
                            .map(|expr| lower_scalar_expression(expr, aggregates, subqueries))
                            .collect()
                    })
                    .collect(),
                alias,
                column_aliases,
            },
            FromClause::Function {
                name,
                args,
                alias,
                column_aliases,
                column_types,
            } => Self::Function {
                name,
                args: args
                    .into_iter()
                    .map(|expr| lower_scalar_expression(expr, aggregates, subqueries))
                    .collect(),
                alias,
                column_aliases,
                column_types,
            },
            FromClause::Subquery {
                body,
                alias,
                column_aliases,
            } => Self::Subquery {
                body: Box::new(QueryPlan::lower_with(*body, aggregates)),
                alias,
                column_aliases,
            },
        }
    }

    fn push_expressions<'a>(&'a self, output: &mut Vec<&'a ScalarExpr>) {
        match self {
            Self::Table { .. } | Self::Subquery { .. } => {}
            Self::Join {
                left, right, on, ..
            } => {
                left.push_expressions(output);
                right.push_expressions(output);
                if let Some(on) = on {
                    output.push(on);
                }
            }
            Self::Values { rows, .. } => {
                for row in rows {
                    output.extend(row);
                }
            }
            Self::Function { args, .. } => output.extend(args),
        }
    }

    pub fn collect_tables(&self, output: &mut Vec<(String, Option<String>)>) {
        match self {
            Self::Table { name, alias } => output.push((name.clone(), alias.clone())),
            Self::Join { left, right, .. } => {
                left.collect_tables(output);
                right.collect_tables(output);
            }
            Self::Values { .. } | Self::Function { .. } | Self::Subquery { .. } => {}
        }
    }
}

impl ProjectionPlan {
    fn lower_with(
        projection: Projection,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Self {
        Self {
            expr: lower_scalar_expression(projection.expr, aggregates, subqueries),
            alias: projection.alias,
        }
    }
}

impl OrderPlan {
    fn lower_with(
        order: OrderBy,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Self {
        Self {
            expr: lower_scalar_expression(order.expr, aggregates, subqueries),
            descending: order.descending,
            nulls: order.nulls,
        }
    }
}

impl ExpressionPlan {
    #[must_use]
    pub fn lower(expression: Expr) -> Self {
        Self::lower_with(expression, &NoRegisteredAggregates)
    }

    fn lower_with(expression: Expr, aggregates: &dyn AggregateClassifier) -> Self {
        let mut subqueries = Vec::new();
        let scalar = lower_scalar_expression(expression, aggregates, &mut subqueries);
        Self { scalar, subqueries }
    }
}

fn lower_scalar_expression(
    expression: Expr,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> ScalarExpr {
    match expression {
        Expr::Star => ScalarExpr::Star,
        Expr::Column(column) => ScalarExpr::Column(column),
        Expr::QualifiedColumn {
            qualifier,
            column,
            key,
        } => ScalarExpr::QualifiedColumn {
            qualifier,
            column,
            key,
        },
        Expr::Literal(value) => ScalarExpr::Literal(value),
        Expr::Param(index) => ScalarExpr::Param(index),
        Expr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => ScalarExpr::Func {
            name,
            args: args
                .into_iter()
                .map(|argument| lower_scalar_expression(argument, aggregates, subqueries))
                .collect(),
            distinct,
            order_by: order_by
                .into_iter()
                .map(|order| lower_scalar_order(order, aggregates, subqueries))
                .collect(),
            filter: filter
                .map(|filter| Box::new(lower_scalar_expression(*filter, aggregates, subqueries))),
        },
        Expr::Array(items) => ScalarExpr::Array(
            items
                .into_iter()
                .map(|item| lower_scalar_expression(item, aggregates, subqueries))
                .collect(),
        ),
        Expr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
            op,
            lhs: Box::new(lower_scalar_expression(*lhs, aggregates, subqueries)),
            rhs: Box::new(lower_scalar_expression(*rhs, aggregates, subqueries)),
        },
        Expr::Not(expression) => ScalarExpr::Not(Box::new(lower_scalar_expression(
            *expression,
            aggregates,
            subqueries,
        ))),
        Expr::And(items) => ScalarExpr::And(
            items
                .into_iter()
                .map(|item| lower_scalar_expression(item, aggregates, subqueries))
                .collect(),
        ),
        Expr::Or(items) => ScalarExpr::Or(
            items
                .into_iter()
                .map(|item| lower_scalar_expression(item, aggregates, subqueries))
                .collect(),
        ),
        Expr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(lower_scalar_expression(*expr, aggregates, subqueries)),
            negated,
        },
        Expr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(lower_scalar_expression(*expr, aggregates, subqueries)),
            low: Box::new(lower_scalar_expression(*low, aggregates, subqueries)),
            high: Box::new(lower_scalar_expression(*high, aggregates, subqueries)),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(lower_scalar_expression(*expr, aggregates, subqueries)),
            list: list
                .into_iter()
                .map(|item| lower_scalar_expression(item, aggregates, subqueries))
                .collect(),
            negated,
        },
        Expr::WindowCall { name, args, spec } => ScalarExpr::WindowCall {
            name,
            args: args
                .into_iter()
                .map(|argument| lower_scalar_expression(argument, aggregates, subqueries))
                .collect(),
            spec: lower_scalar_window_spec(spec, aggregates, subqueries),
        },
        Expr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: base.map(|base| Box::new(lower_scalar_expression(*base, aggregates, subqueries))),
            when: when
                .into_iter()
                .map(|(condition, result)| {
                    (
                        lower_scalar_expression(condition, aggregates, subqueries),
                        lower_scalar_expression(result, aggregates, subqueries),
                    )
                })
                .collect(),
            else_branch: else_branch
                .map(|branch| Box::new(lower_scalar_expression(*branch, aggregates, subqueries))),
        },
        Expr::Cast { expr, ty } => ScalarExpr::Cast {
            expr: Box::new(lower_scalar_expression(*expr, aggregates, subqueries)),
            ty,
        },
        Expr::ScalarSubquery(query) => {
            let id = subqueries.len();
            subqueries.push(QueryPlan::lower_with(*query, aggregates));
            ScalarExpr::ScalarSubquery(id)
        }
        Expr::Exists { body, negated } => {
            let id = subqueries.len();
            subqueries.push(QueryPlan::lower_with(*body, aggregates));
            ScalarExpr::Exists {
                subquery: id,
                negated,
            }
        }
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => {
            let expression = Box::new(lower_scalar_expression(*expr, aggregates, subqueries));
            let id = subqueries.len();
            subqueries.push(QueryPlan::lower_with(*body, aggregates));
            ScalarExpr::InSubquery {
                expr: expression,
                subquery: id,
                negated,
            }
        }
    }
}

fn lower_scalar_order(
    order: OrderBy,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> ScalarOrder {
    ScalarOrder {
        expr: lower_scalar_expression(order.expr, aggregates, subqueries),
        descending: order.descending,
        nulls: order.nulls,
    }
}

fn lower_scalar_window_spec(
    spec: WindowSpec,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> ScalarWindowSpec {
    ScalarWindowSpec {
        partition_by: spec
            .partition_by
            .into_iter()
            .map(|expression| lower_scalar_expression(expression, aggregates, subqueries))
            .collect(),
        order_by: spec
            .order_by
            .into_iter()
            .map(|order| lower_scalar_order(order, aggregates, subqueries))
            .collect(),
        frame: spec
            .frame
            .map(|frame| lower_scalar_window_frame(frame, aggregates, subqueries)),
    }
}

fn lower_scalar_window_frame(
    frame: uqa_sql::ast::WindowFrame,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> ScalarWindowFrame {
    ScalarWindowFrame {
        mode: frame.mode,
        start: lower_scalar_frame_bound(frame.start, aggregates, subqueries),
        end: lower_scalar_frame_bound(frame.end, aggregates, subqueries),
    }
}

fn lower_scalar_frame_bound(
    bound: FrameBound,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> ScalarFrameBound {
    match bound {
        FrameBound::UnboundedPreceding => ScalarFrameBound::UnboundedPreceding,
        FrameBound::UnboundedFollowing => ScalarFrameBound::UnboundedFollowing,
        FrameBound::CurrentRow => ScalarFrameBound::CurrentRow,
        FrameBound::Preceding(expression) => ScalarFrameBound::Preceding(Box::new(
            lower_scalar_expression(*expression, aggregates, subqueries),
        )),
        FrameBound::Following(expression) => ScalarFrameBound::Following(Box::new(
            lower_scalar_expression(*expression, aggregates, subqueries),
        )),
    }
}

pub(crate) fn is_builtin_aggregate(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "count"
            | "sum"
            | "avg"
            | "min"
            | "max"
            | "string_agg"
            | "array_agg"
            | "bool_and"
            | "bool_or"
            | "stddev"
            | "stddev_samp"
            | "stddev_pop"
            | "variance"
            | "var_samp"
            | "var_pop"
            | "percentile_cont"
            | "percentile_disc"
            | "mode"
            | "json_agg"
            | "jsonb_agg"
            | "json_object_agg"
            | "jsonb_object_agg"
    )
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

#[cfg(test)]
mod tests {
    use uqa_sql::compile;

    use super::{CommandPlan, ComputePlan, RelationalPlan, SourcePlan, UnifiedPlan};

    fn one(sql: &str) -> UnifiedPlan {
        let mut statements = compile(sql).expect("SQL compiles");
        assert_eq!(statements.len(), 1);
        UnifiedPlan::lower(statements.remove(0))
    }

    #[test]
    fn arithmetic_and_window_are_relational_compute_nodes() {
        let arithmetic = one("SELECT a + 1 AS b FROM t");
        let UnifiedPlan::Query(query) = arithmetic else {
            panic!("expected query plan");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("expected query block");
        };
        assert!(matches!(block.compute, ComputePlan::Project));

        let window = one("SELECT row_number() OVER (ORDER BY a) AS n FROM t");
        let UnifiedPlan::Query(query) = window else {
            panic!("expected query plan");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("expected query block");
        };
        assert!(matches!(block.compute, ComputePlan::Window));
    }

    #[test]
    fn from_and_scalar_subqueries_own_query_children() {
        let plan =
            one("SELECT (SELECT max(x) FROM inner_t) AS m FROM (SELECT y FROM outer_t) AS s");
        let UnifiedPlan::Query(query) = plan else {
            panic!("expected query plan");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("expected query block");
        };
        assert!(matches!(block.from, Some(SourcePlan::Subquery { .. })));
        assert_eq!(block.subqueries.len(), 1);
    }

    #[test]
    fn set_operations_and_ctes_are_structural_children() {
        let plan = one("WITH q AS (SELECT 1 AS x) SELECT x FROM q UNION SELECT 2");
        let UnifiedPlan::Query(query) = plan else {
            panic!("expected query plan");
        };
        assert_eq!(query.ctes.len(), 1);
        assert!(matches!(query.root, RelationalPlan::SetOp { .. }));
    }

    #[test]
    fn values_is_a_query_plan_not_a_command_escape_hatch() {
        let plan = one("VALUES (1 + 2), (3 + 4)");
        let UnifiedPlan::Query(query) = plan else {
            panic!("VALUES must be relational");
        };
        assert!(matches!(query.root, RelationalPlan::Values { .. }));
    }

    #[test]
    fn mutations_own_source_and_scalar_query_children() {
        let update = one("WITH limits AS (SELECT max(v) AS v FROM source) \
             UPDATE target SET v = (SELECT v FROM limits) FROM source \
             WHERE target.id = source.id");
        let UnifiedPlan::Command(update) = update else {
            panic!("UPDATE must be a command plan");
        };
        let CommandPlan::Update(update) = update.as_ref() else {
            panic!("expected UPDATE plan");
        };
        assert_eq!(update.ctes.len(), 1);
        assert!(matches!(
            update.source.as_deref(),
            Some(SourcePlan::Table { .. })
        ));
        assert_eq!(update.subqueries.len(), 1);

        let merge = one("MERGE INTO target USING (SELECT id, v FROM source) AS s \
             ON target.id = s.id WHEN MATCHED THEN UPDATE SET v = s.v");
        let UnifiedPlan::Command(merge) = merge else {
            panic!("MERGE must be a command plan");
        };
        let CommandPlan::Merge(merge) = merge.as_ref() else {
            panic!("expected MERGE plan");
        };
        assert!(matches!(merge.source.as_ref(), SourcePlan::Subquery { .. }));
    }
}
