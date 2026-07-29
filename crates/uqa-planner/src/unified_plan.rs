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

use std::time::Instant;

use uqa_sql::ast::{
    Expr, FrameBound, FromClause, NullsOrder, OrderBy, Projection, SelectStmt, SetOpKind,
    Statement, WindowSpec, CTE,
};
use uqa_sql::SQLResult;

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
#[derive(Debug, Clone)]
pub struct QueryPlan {
    pub ctes: Vec<CtePlan>,
    pub root: RelationalPlan,
}

/// A named query child owned by a [`QueryPlan`].
#[derive(Debug, Clone)]
pub struct CtePlan {
    pub name: String,
    pub columns: Vec<String>,
    pub recursive: bool,
    pub query: Box<QueryPlan>,
}

/// Relational nodes common to ordinary SQL, retrieval SQL, and table/graph
/// functions.
#[derive(Debug, Clone)]
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
        limit: Option<Box<ExpressionPlan>>,
        offset: Option<Box<ExpressionPlan>>,
    },
    /// Standalone `VALUES`, used both as a statement and as a relational
    /// source. Each cell remains an expression so parameters/functions bind at
    /// execution time.
    Values { rows: Vec<Vec<ExpressionPlan>> },
}

/// One SELECT block after `WITH` and set-operation structure has been pulled
/// into explicit parent/child nodes.
#[derive(Debug, Clone)]
pub struct QueryBlockPlan {
    pub source: SourcePlan,
    pub filter: Option<ExpressionPlan>,
    pub compute: ComputePlan,
    pub order_by: Vec<OrderPlan>,
    pub limit: Option<ExpressionPlan>,
    pub offset: Option<ExpressionPlan>,
    pub distinct: bool,
    pub distinct_on: Vec<ExpressionPlan>,
}

/// The row-producing source below a query block.
#[derive(Debug, Clone)]
pub enum SourcePlan {
    /// `SELECT ...` without `FROM` starts from exactly one empty row.
    OneRow,
    Table {
        name: String,
        alias: Option<String>,
    },
    Join {
        left: Box<SourcePlan>,
        right: Box<SourcePlan>,
        kind: uqa_sql::ast::JoinKind,
        on: Option<ExpressionPlan>,
        lateral: bool,
    },
    Values {
        rows: Vec<Vec<ExpressionPlan>>,
        alias: Option<String>,
        column_aliases: Vec<String>,
    },
    Function {
        name: String,
        args: Vec<ExpressionPlan>,
        alias: Option<String>,
        column_aliases: Vec<String>,
        column_types: Vec<String>,
    },
    Subquery {
        query: Box<QueryPlan>,
        alias: Option<String>,
        column_aliases: Vec<String>,
    },
}

/// The SELECT-list phase chosen during lowering.
#[derive(Debug, Clone)]
pub enum ComputePlan {
    Project {
        projections: Vec<ProjectionPlan>,
    },
    Aggregate {
        projections: Vec<ProjectionPlan>,
        group_by: Vec<ExpressionPlan>,
        grouping_sets: Vec<Vec<ExpressionPlan>>,
        having: Option<Box<ExpressionPlan>>,
    },
    Window {
        projections: Vec<ProjectionPlan>,
    },
}

impl ComputePlan {
    #[must_use]
    pub fn projections(&self) -> &[ProjectionPlan] {
        match self {
            Self::Project { projections }
            | Self::Aggregate { projections, .. }
            | Self::Window { projections } => projections,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProjectionPlan {
    pub expression: ExpressionPlan,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OrderPlan {
    pub expression: ExpressionPlan,
    pub descending: bool,
    pub nulls: Option<NullsOrder>,
}

/// Scalar-expression IR plus every query-valued descendant it owns.
///
/// The original expression is retained because the scalar evaluator already
/// implements SQL coercion and three-valued logic. `subqueries` is the
/// authoritative list of nested relational plans; physical evaluation must
/// execute those plans rather than dispatch the embedded `SelectStmt`
/// directly.
#[derive(Debug, Clone)]
pub struct ExpressionPlan {
    pub expression: Expr,
    pub subqueries: Vec<QueryPlan>,
}

/// Non-query statement plans. Query-bearing commands carry an explicit child
/// plan alongside the command payload used by the mutation/catalog runtime.
#[derive(Debug, Clone)]
pub enum CommandPlan {
    CreateTable(uqa_sql::ast::CreateTable),
    CreateIndex(uqa_sql::ast::CreateIndex),
    Insert {
        statement: Box<uqa_sql::ast::InsertStmt>,
        source: Option<Box<QueryPlan>>,
        expressions: Vec<ExpressionPlan>,
    },
    Update {
        statement: Box<uqa_sql::ast::UpdateStmt>,
        ctes: Vec<CtePlan>,
        source: Option<Box<SourcePlan>>,
        expressions: Vec<ExpressionPlan>,
    },
    Delete {
        statement: Box<uqa_sql::ast::DeleteStmt>,
        ctes: Vec<CtePlan>,
        source: Option<Box<SourcePlan>>,
        expressions: Vec<ExpressionPlan>,
    },
    Drop(uqa_sql::ast::DropStmt),
    AlterTable(Box<uqa_sql::ast::AlterTableStmt>),
    CreateView {
        name: String,
        definition: Box<SelectStmt>,
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
        definition: Box<Statement>,
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
    Merge {
        statement: Box<uqa_sql::ast::MergeStmt>,
        source: Box<SourcePlan>,
        expressions: Vec<ExpressionPlan>,
    },
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
            Statement::Values { rows } => Self::Query(Box::new(QueryPlan {
                ctes: Vec::new(),
                root: RelationalPlan::Values {
                    rows: rows
                        .into_iter()
                        .map(|row| {
                            row.into_iter()
                                .map(|expr| ExpressionPlan::lower_with(expr, aggregates))
                                .collect()
                        })
                        .collect(),
                },
            })),
            Statement::CreateTable(value) => {
                Self::Command(Box::new(CommandPlan::CreateTable(value)))
            }
            Statement::CreateIndex(value) => {
                Self::Command(Box::new(CommandPlan::CreateIndex(value)))
            }
            Statement::Insert(statement) => {
                let source = statement.select_source.as_ref().map(|query| {
                    let mut query = (**query).clone();
                    if !statement.with.is_empty() {
                        let mut inherited = statement.with.clone();
                        inherited.extend(query.with);
                        query.with = inherited;
                    }
                    Box::new(QueryPlan::lower_with(query, aggregates))
                });
                let expressions = lower_expression_refs(insert_expressions(&statement), aggregates);
                Self::Command(Box::new(CommandPlan::Insert {
                    statement: Box::new(statement),
                    source,
                    expressions,
                }))
            }
            Statement::Update(statement) => {
                let ctes = lower_ctes(&statement.with, aggregates);
                let source = statement
                    .from
                    .clone()
                    .map(|from| SourcePlan::lower_with(from, aggregates));
                let expressions = lower_expression_refs(update_expressions(&statement), aggregates);
                Self::Command(Box::new(CommandPlan::Update {
                    statement: Box::new(statement),
                    ctes,
                    source: source.map(Box::new),
                    expressions,
                }))
            }
            Statement::Delete(statement) => {
                let ctes = lower_ctes(&statement.with, aggregates);
                let source = statement
                    .using
                    .clone()
                    .map(|from| SourcePlan::lower_with(from, aggregates));
                let expressions = lower_expression_refs(delete_expressions(&statement), aggregates);
                Self::Command(Box::new(CommandPlan::Delete {
                    statement: Box::new(statement),
                    ctes,
                    source: source.map(Box::new),
                    expressions,
                }))
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
                let definition = *body;
                let query = Box::new(QueryPlan::lower_with(definition.clone(), aggregates));
                Self::Command(Box::new(CommandPlan::CreateView {
                    name,
                    definition: Box::new(definition),
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
                let definition = *body;
                let body = Box::new(Self::lower_with(definition.clone(), aggregates));
                Self::Command(Box::new(CommandPlan::Prepare {
                    name,
                    definition: Box::new(definition),
                    body,
                }))
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
                let source = SourcePlan::lower_with(statement.source.clone(), aggregates);
                let expressions = lower_expression_refs(merge_expressions(&statement), aggregates);
                Self::Command(Box::new(CommandPlan::Merge {
                    statement: Box::new(statement),
                    source: Box::new(source),
                    expressions,
                }))
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

    /// Rebuild the SQL expression carrier consumed by the row-oriented
    /// physical operators. The relational structure comes from this plan;
    /// this method does not parse, optimise, or dispatch a statement.
    #[must_use]
    pub fn physical_select(&self) -> Option<SelectStmt> {
        let mut statement = match &self.root {
            RelationalPlan::QueryBlock(block) => block.physical_select(),
            RelationalPlan::SetOp {
                kind,
                all,
                left,
                right,
                order_by,
                limit,
                offset,
            } => SelectStmt {
                projections: Vec::new(),
                from: None,
                r#where: None,
                group_by: Vec::new(),
                grouping_sets: Vec::new(),
                having: None,
                order_by: Vec::new(),
                limit: None,
                offset: None,
                with: Vec::new(),
                set_op: Some(Box::new(uqa_sql::ast::SetOp {
                    kind: *kind,
                    all: *all,
                    left: Some(Box::new(left.physical_select()?)),
                    right: right.physical_select()?,
                    combined_order_by: order_by.iter().map(OrderPlan::physical_order).collect(),
                    combined_limit: limit
                        .as_ref()
                        .map(|expression| expression.expression.clone()),
                    combined_offset: offset
                        .as_ref()
                        .map(|expression| expression.expression.clone()),
                })),
                distinct: false,
                distinct_on: Vec::new(),
            },
            RelationalPlan::Values { .. } => return None,
        };
        statement.with = self.ctes.iter().filter_map(CtePlan::physical_cte).collect();
        Some(statement)
    }
}

impl CtePlan {
    fn physical_cte(&self) -> Option<CTE> {
        Some(CTE {
            name: self.name.clone(),
            columns: self.columns.clone(),
            recursive: self.recursive,
            query: Box::new(self.query.physical_select()?),
        })
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

fn lower_expression_refs(
    expressions: Vec<&Expr>,
    aggregates: &dyn AggregateClassifier,
) -> Vec<ExpressionPlan> {
    expressions
        .into_iter()
        .map(|expression| ExpressionPlan::lower_with(expression.clone(), aggregates))
        .collect()
}

fn insert_expressions(statement: &uqa_sql::ast::InsertStmt) -> Vec<&Expr> {
    let mut expressions = Vec::new();
    for row in &statement.rows {
        expressions.extend(row);
    }
    if let Some(uqa_sql::ast::OnConflict {
        action:
            uqa_sql::ast::OnConflictAction::Update {
                assignments,
                r#where,
            },
        ..
    }) = &statement.on_conflict
    {
        expressions.extend(assignments.iter().map(|(_, expression)| expression));
        expressions.extend(r#where);
    }
    expressions.extend(
        statement
            .returning
            .iter()
            .map(|projection| &projection.expr),
    );
    expressions
}

fn update_expressions(statement: &uqa_sql::ast::UpdateStmt) -> Vec<&Expr> {
    let mut expressions: Vec<&Expr> = statement
        .assignments
        .iter()
        .map(|(_, expression)| expression)
        .collect();
    expressions.extend(&statement.r#where);
    expressions.extend(
        statement
            .returning
            .iter()
            .map(|projection| &projection.expr),
    );
    expressions
}

fn delete_expressions(statement: &uqa_sql::ast::DeleteStmt) -> Vec<&Expr> {
    let mut expressions = Vec::new();
    expressions.extend(&statement.r#where);
    expressions.extend(
        statement
            .returning
            .iter()
            .map(|projection| &projection.expr),
    );
    expressions
}

fn merge_expressions(statement: &uqa_sql::ast::MergeStmt) -> Vec<&Expr> {
    let mut expressions = vec![&statement.join_condition];
    for clause in &statement.when_clauses {
        match clause {
            uqa_sql::ast::MergeWhen::UpdateMatched {
                condition,
                assignments,
            } => {
                expressions.extend(condition);
                expressions.extend(assignments.iter().map(|(_, expression)| expression));
            }
            uqa_sql::ast::MergeWhen::DeleteMatched { condition }
            | uqa_sql::ast::MergeWhen::NothingMatched { condition }
            | uqa_sql::ast::MergeWhen::NothingNotMatched { condition } => {
                expressions.extend(condition);
            }
            uqa_sql::ast::MergeWhen::InsertNotMatched {
                condition, values, ..
            } => {
                expressions.extend(condition);
                expressions.extend(values);
            }
        }
    }
    expressions.extend(
        statement
            .returning
            .iter()
            .map(|projection| &projection.expr),
    );
    expressions
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
    RelationalPlan::SetOp {
        kind: set_op.kind,
        all: set_op.all,
        left: Box::new(left),
        right: Box::new(right),
        order_by: set_op
            .combined_order_by
            .into_iter()
            .map(|order| OrderPlan::lower_with(order, aggregates))
            .collect(),
        limit: set_op
            .combined_limit
            .map(|expr| Box::new(ExpressionPlan::lower_with(expr, aggregates))),
        offset: set_op
            .combined_offset
            .map(|expr| Box::new(ExpressionPlan::lower_with(expr, aggregates))),
    }
}

impl QueryBlockPlan {
    fn lower_with(statement: SelectStmt, aggregates: &dyn AggregateClassifier) -> Self {
        debug_assert!(statement.with.is_empty());
        debug_assert!(statement.set_op.is_none());
        let projections: Vec<ProjectionPlan> = statement
            .projections
            .into_iter()
            .map(|projection| ProjectionPlan::lower_with(projection, aggregates))
            .collect();
        let has_aggregate = !statement.group_by.is_empty()
            || !statement.grouping_sets.is_empty()
            || statement.having.is_some()
            || projections.iter().any(|projection| {
                expression_contains_aggregate(&projection.expression.expression, aggregates)
            });
        let has_window = projections
            .iter()
            .any(|projection| expression_contains_window(&projection.expression.expression));
        let compute = if has_aggregate {
            ComputePlan::Aggregate {
                projections,
                group_by: statement
                    .group_by
                    .into_iter()
                    .map(|expr| ExpressionPlan::lower_with(expr, aggregates))
                    .collect(),
                grouping_sets: statement
                    .grouping_sets
                    .into_iter()
                    .map(|set| {
                        set.into_iter()
                            .map(|expr| ExpressionPlan::lower_with(expr, aggregates))
                            .collect()
                    })
                    .collect(),
                having: statement
                    .having
                    .map(|expr| Box::new(ExpressionPlan::lower_with(expr, aggregates))),
            }
        } else if has_window {
            ComputePlan::Window { projections }
        } else {
            ComputePlan::Project { projections }
        };
        Self {
            source: statement.from.map_or(SourcePlan::OneRow, |source| {
                SourcePlan::lower_with(source, aggregates)
            }),
            filter: statement
                .r#where
                .map(|expr| ExpressionPlan::lower_with(expr, aggregates)),
            compute,
            order_by: statement
                .order_by
                .into_iter()
                .map(|order| OrderPlan::lower_with(order, aggregates))
                .collect(),
            limit: statement
                .limit
                .map(|expr| ExpressionPlan::lower_with(expr, aggregates)),
            offset: statement
                .offset
                .map(|expr| ExpressionPlan::lower_with(expr, aggregates)),
            distinct: statement.distinct,
            distinct_on: statement
                .distinct_on
                .into_iter()
                .map(|expr| ExpressionPlan::lower_with(expr, aggregates))
                .collect(),
        }
    }

    #[must_use]
    pub fn physical_select(&self) -> SelectStmt {
        let (projections, group_by, grouping_sets, having) = match &self.compute {
            ComputePlan::Project { projections } | ComputePlan::Window { projections } => (
                projections
                    .iter()
                    .map(ProjectionPlan::physical_projection)
                    .collect(),
                Vec::new(),
                Vec::new(),
                None,
            ),
            ComputePlan::Aggregate {
                projections,
                group_by,
                grouping_sets,
                having,
            } => (
                projections
                    .iter()
                    .map(ProjectionPlan::physical_projection)
                    .collect(),
                group_by
                    .iter()
                    .map(|expression| expression.expression.clone())
                    .collect(),
                grouping_sets
                    .iter()
                    .map(|set| {
                        set.iter()
                            .map(|expression| expression.expression.clone())
                            .collect()
                    })
                    .collect(),
                having
                    .as_ref()
                    .map(|expression| expression.expression.clone()),
            ),
        };
        SelectStmt {
            projections,
            from: self.source.physical_from(),
            r#where: self
                .filter
                .as_ref()
                .map(|expression| expression.expression.clone()),
            group_by,
            grouping_sets,
            having,
            order_by: self
                .order_by
                .iter()
                .map(OrderPlan::physical_order)
                .collect(),
            limit: self
                .limit
                .as_ref()
                .map(|expression| expression.expression.clone()),
            offset: self
                .offset
                .as_ref()
                .map(|expression| expression.expression.clone()),
            with: Vec::new(),
            set_op: None,
            distinct: self.distinct,
            distinct_on: self
                .distinct_on
                .iter()
                .map(|expression| expression.expression.clone())
                .collect(),
        }
    }

    /// Expression nodes evaluated while executing this query block. Query
    /// bodies under `FROM (SELECT ...)` are excluded because their child plan
    /// installs its own expression scope when it executes.
    #[must_use]
    pub fn expressions(&self) -> Vec<&ExpressionPlan> {
        let mut expressions = Vec::new();
        self.source.push_expressions(&mut expressions);
        if let Some(filter) = &self.filter {
            expressions.push(filter);
        }
        for projection in self.compute.projections() {
            expressions.push(&projection.expression);
        }
        if let ComputePlan::Aggregate {
            group_by,
            grouping_sets,
            having,
            ..
        } = &self.compute
        {
            expressions.extend(group_by);
            for set in grouping_sets {
                expressions.extend(set);
            }
            if let Some(having) = having {
                expressions.push(having);
            }
        }
        expressions.extend(self.order_by.iter().map(|order| &order.expression));
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
    fn lower_with(source: FromClause, aggregates: &dyn AggregateClassifier) -> Self {
        match source {
            FromClause::Table { name, alias } => Self::Table { name, alias },
            FromClause::Join {
                left,
                right,
                kind,
                on,
                lateral,
            } => Self::Join {
                left: Box::new(Self::lower_with(*left, aggregates)),
                right: Box::new(Self::lower_with(*right, aggregates)),
                kind,
                on: on.map(|expr| ExpressionPlan::lower_with(expr, aggregates)),
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
                            .map(|expr| ExpressionPlan::lower_with(expr, aggregates))
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
                    .map(|expr| ExpressionPlan::lower_with(expr, aggregates))
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
                query: Box::new(QueryPlan::lower_with(*body, aggregates)),
                alias,
                column_aliases,
            },
        }
    }

    #[must_use]
    pub fn physical_from(&self) -> Option<FromClause> {
        match self {
            Self::OneRow => None,
            Self::Table { name, alias } => Some(FromClause::Table {
                name: name.clone(),
                alias: alias.clone(),
            }),
            Self::Join {
                left,
                right,
                kind,
                on,
                lateral,
            } => Some(FromClause::Join {
                left: Box::new(left.physical_from()?),
                right: Box::new(right.physical_from()?),
                kind: *kind,
                on: on.as_ref().map(|expression| expression.expression.clone()),
                lateral: *lateral,
            }),
            Self::Values {
                rows,
                alias,
                column_aliases,
            } => Some(FromClause::Values {
                rows: rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|expression| expression.expression.clone())
                            .collect()
                    })
                    .collect(),
                alias: alias.clone(),
                column_aliases: column_aliases.clone(),
            }),
            Self::Function {
                name,
                args,
                alias,
                column_aliases,
                column_types,
            } => Some(FromClause::Function {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|expression| expression.expression.clone())
                    .collect(),
                alias: alias.clone(),
                column_aliases: column_aliases.clone(),
                column_types: column_types.clone(),
            }),
            Self::Subquery {
                query,
                alias,
                column_aliases,
            } => Some(FromClause::Subquery {
                body: Box::new(query.physical_select()?),
                alias: alias.clone(),
                column_aliases: column_aliases.clone(),
            }),
        }
    }

    fn push_expressions<'a>(&'a self, output: &mut Vec<&'a ExpressionPlan>) {
        match self {
            Self::OneRow | Self::Table { .. } | Self::Subquery { .. } => {}
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

    /// Stable physical-source keys paired with the already-lowered query
    /// children behind `FROM (SELECT ...)`. The row source executor uses
    /// these bindings instead of lowering the embedded carrier again.
    #[must_use]
    pub fn subquery_bindings(&self) -> Vec<(String, &QueryPlan)> {
        let mut output = Vec::new();
        self.push_subquery_bindings(&mut output);
        output
    }

    fn push_subquery_bindings<'a>(&'a self, output: &mut Vec<(String, &'a QueryPlan)>) {
        match self {
            Self::Join { left, right, .. } => {
                left.push_subquery_bindings(output);
                right.push_subquery_bindings(output);
            }
            Self::Subquery { query, .. } => {
                if let Some(statement) = query.physical_select() {
                    output.push((format!("{statement:?}"), query));
                }
            }
            Self::OneRow | Self::Table { .. } | Self::Values { .. } | Self::Function { .. } => {}
        }
    }
}

impl ProjectionPlan {
    fn lower_with(projection: Projection, aggregates: &dyn AggregateClassifier) -> Self {
        Self {
            expression: ExpressionPlan::lower_with(projection.expr, aggregates),
            alias: projection.alias,
        }
    }

    fn physical_projection(&self) -> Projection {
        Projection {
            expr: self.expression.expression.clone(),
            alias: self.alias.clone(),
        }
    }
}

impl OrderPlan {
    fn lower_with(order: OrderBy, aggregates: &dyn AggregateClassifier) -> Self {
        Self {
            expression: ExpressionPlan::lower_with(order.expr, aggregates),
            descending: order.descending,
            nulls: order.nulls,
        }
    }

    fn physical_order(&self) -> OrderBy {
        OrderBy {
            expr: self.expression.expression.clone(),
            descending: self.descending,
            nulls: self.nulls,
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
        collect_expression_subqueries(&expression, aggregates, &mut subqueries);
        Self {
            expression,
            subqueries,
        }
    }

    /// Stable expression-subquery keys paired with their already-lowered
    /// child plans. The engine uses these bindings when the scalar evaluator
    /// calls its subquery hook.
    #[must_use]
    pub fn subquery_bindings(&self) -> Vec<(String, &QueryPlan)> {
        let mut plans = self.subqueries.iter();
        let mut output = Vec::with_capacity(self.subqueries.len());
        collect_subquery_bindings(&self.expression, &mut plans, &mut output);
        debug_assert!(plans.next().is_none());
        output
    }
}

fn collect_subquery_bindings<'a>(
    expression: &Expr,
    plans: &mut std::slice::Iter<'a, QueryPlan>,
    output: &mut Vec<(String, &'a QueryPlan)>,
) {
    match expression {
        Expr::ScalarSubquery(query) | Expr::Exists { body: query, .. } => {
            if let Some(plan) = plans.next() {
                output.push((format!("{query:?}"), plan));
            }
        }
        Expr::InSubquery { expr, body, .. } => {
            collect_subquery_bindings(expr, plans, output);
            if let Some(plan) = plans.next() {
                output.push((format!("{body:?}"), plan));
            }
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_subquery_bindings(item, plans, output);
            }
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                collect_subquery_bindings(argument, plans, output);
            }
            for order in order_by {
                collect_subquery_bindings(&order.expr, plans, output);
            }
            if let Some(filter) = filter {
                collect_subquery_bindings(filter, plans, output);
            }
        }
        Expr::WindowCall { args, spec, .. } => {
            for argument in args {
                collect_subquery_bindings(argument, plans, output);
            }
            for expression in &spec.partition_by {
                collect_subquery_bindings(expression, plans, output);
            }
            for order in &spec.order_by {
                collect_subquery_bindings(&order.expr, plans, output);
            }
            if let Some(frame) = &spec.frame {
                collect_frame_bindings(&frame.start, plans, output);
                collect_frame_bindings(&frame.end, plans, output);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_subquery_bindings(lhs, plans, output);
            collect_subquery_bindings(rhs, plans, output);
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            collect_subquery_bindings(inner, plans, output);
        }
        Expr::Between { expr, low, high } => {
            collect_subquery_bindings(expr, plans, output);
            collect_subquery_bindings(low, plans, output);
            collect_subquery_bindings(high, plans, output);
        }
        Expr::InList { expr, list, .. } => {
            collect_subquery_bindings(expr, plans, output);
            for item in list {
                collect_subquery_bindings(item, plans, output);
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                collect_subquery_bindings(base, plans, output);
            }
            for (condition, result) in when {
                collect_subquery_bindings(condition, plans, output);
                collect_subquery_bindings(result, plans, output);
            }
            if let Some(branch) = else_branch {
                collect_subquery_bindings(branch, plans, output);
            }
        }
        Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_) => {}
    }
}

fn collect_frame_bindings<'a>(
    bound: &FrameBound,
    plans: &mut std::slice::Iter<'a, QueryPlan>,
    output: &mut Vec<(String, &'a QueryPlan)>,
) {
    if let FrameBound::Preceding(expression) | FrameBound::Following(expression) = bound {
        collect_subquery_bindings(expression, plans, output);
    }
}

fn collect_expression_subqueries(
    expression: &Expr,
    aggregates: &dyn AggregateClassifier,
    output: &mut Vec<QueryPlan>,
) {
    match expression {
        Expr::ScalarSubquery(query) | Expr::Exists { body: query, .. } => {
            output.push(QueryPlan::lower_with((**query).clone(), aggregates));
        }
        Expr::InSubquery { expr, body, .. } => {
            collect_expression_subqueries(expr, aggregates, output);
            output.push(QueryPlan::lower_with((**body).clone(), aggregates));
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_expression_subqueries(item, aggregates, output);
            }
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                collect_expression_subqueries(argument, aggregates, output);
            }
            for order in order_by {
                collect_expression_subqueries(&order.expr, aggregates, output);
            }
            if let Some(filter) = filter {
                collect_expression_subqueries(filter, aggregates, output);
            }
        }
        Expr::WindowCall { args, spec, .. } => {
            for argument in args {
                collect_expression_subqueries(argument, aggregates, output);
            }
            for expression in &spec.partition_by {
                collect_expression_subqueries(expression, aggregates, output);
            }
            for order in &spec.order_by {
                collect_expression_subqueries(&order.expr, aggregates, output);
            }
            if let Some(frame) = &spec.frame {
                collect_frame_subqueries(&frame.start, aggregates, output);
                collect_frame_subqueries(&frame.end, aggregates, output);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expression_subqueries(lhs, aggregates, output);
            collect_expression_subqueries(rhs, aggregates, output);
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            collect_expression_subqueries(inner, aggregates, output);
        }
        Expr::Between { expr, low, high } => {
            collect_expression_subqueries(expr, aggregates, output);
            collect_expression_subqueries(low, aggregates, output);
            collect_expression_subqueries(high, aggregates, output);
        }
        Expr::InList { expr, list, .. } => {
            collect_expression_subqueries(expr, aggregates, output);
            for item in list {
                collect_expression_subqueries(item, aggregates, output);
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                collect_expression_subqueries(base, aggregates, output);
            }
            for (condition, result) in when {
                collect_expression_subqueries(condition, aggregates, output);
                collect_expression_subqueries(result, aggregates, output);
            }
            if let Some(branch) = else_branch {
                collect_expression_subqueries(branch, aggregates, output);
            }
        }
        Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_) => {}
    }
}

fn collect_frame_subqueries(
    bound: &FrameBound,
    aggregates: &dyn AggregateClassifier,
    output: &mut Vec<QueryPlan>,
) {
    if let FrameBound::Preceding(expression) | FrameBound::Following(expression) = bound {
        collect_expression_subqueries(expression, aggregates, output);
    }
}

fn expression_contains_window(expression: &Expr) -> bool {
    match expression {
        Expr::WindowCall { .. } => true,
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
            items.iter().any(expression_contains_window)
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(expression_contains_window)
                || order_by
                    .iter()
                    .any(|order| expression_contains_window(&order.expr))
                || filter.as_deref().is_some_and(expression_contains_window)
        }
        Expr::Binary { lhs, rhs, .. } => {
            expression_contains_window(lhs) || expression_contains_window(rhs)
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            expression_contains_window(inner)
        }
        Expr::Between { expr, low, high } => {
            expression_contains_window(expr)
                || expression_contains_window(low)
                || expression_contains_window(high)
        }
        Expr::InList { expr, list, .. } => {
            expression_contains_window(expr) || list.iter().any(expression_contains_window)
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_some_and(expression_contains_window)
                || when.iter().any(|(condition, result)| {
                    expression_contains_window(condition) || expression_contains_window(result)
                })
                || else_branch
                    .as_deref()
                    .is_some_and(expression_contains_window)
        }
        Expr::InSubquery { expr, .. } => expression_contains_window(expr),
        Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_) => false,
    }
}

fn expression_contains_aggregate(expression: &Expr, aggregates: &dyn AggregateClassifier) -> bool {
    match expression {
        Expr::Func {
            name, args, filter, ..
        } => {
            is_builtin_aggregate(name)
                || aggregates.is_registered_aggregate(name)
                || args
                    .iter()
                    .any(|expr| expression_contains_aggregate(expr, aggregates))
                || filter
                    .as_deref()
                    .is_some_and(|expr| expression_contains_aggregate(expr, aggregates))
        }
        Expr::Array(items) | Expr::And(items) | Expr::Or(items) => items
            .iter()
            .any(|expr| expression_contains_aggregate(expr, aggregates)),
        Expr::Binary { lhs, rhs, .. } => {
            expression_contains_aggregate(lhs, aggregates)
                || expression_contains_aggregate(rhs, aggregates)
        }
        Expr::Not(inner) | Expr::IsNull { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            expression_contains_aggregate(inner, aggregates)
        }
        Expr::Between { expr, low, high } => {
            expression_contains_aggregate(expr, aggregates)
                || expression_contains_aggregate(low, aggregates)
                || expression_contains_aggregate(high, aggregates)
        }
        Expr::InList { expr, list, .. } => {
            expression_contains_aggregate(expr, aggregates)
                || list
                    .iter()
                    .any(|item| expression_contains_aggregate(item, aggregates))
        }
        Expr::WindowCall { .. } => false,
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref()
                .is_some_and(|expr| expression_contains_aggregate(expr, aggregates))
                || when.iter().any(|(condition, result)| {
                    expression_contains_aggregate(condition, aggregates)
                        || expression_contains_aggregate(result, aggregates)
                })
                || else_branch
                    .as_deref()
                    .is_some_and(|expr| expression_contains_aggregate(expr, aggregates))
        }
        Expr::InSubquery { expr, .. } => expression_contains_aggregate(expr, aggregates),
        Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::Star
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Literal(_)
        | Expr::Param(_) => false,
    }
}

fn is_builtin_aggregate(name: &str) -> bool {
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
            Self::Insert { .. } => "Insert",
            Self::Update { .. } => "Update",
            Self::Delete { .. } => "Delete",
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
            Self::Merge { .. } => "Merge",
            Self::CreateFunction(_) => "CreateFunction",
            Self::DropFunction(_) => "DropFunction",
            Self::DoBlock { .. } => "DoBlock",
            Self::Call { .. } => "Call",
        }
    }
}

/// Runtime interface for the one top-level plan executor.
pub trait UnifiedPlanDriver {
    type Error;

    fn execute_plan(&self, plan: &UnifiedPlan) -> Result<SQLResult, Self::Error>;
}

/// Root statistics for SQL plan execution.
#[derive(Debug, Clone, Default)]
pub struct UnifiedExecutionStats {
    pub plan_name: String,
    pub elapsed_ms: f64,
    pub result_rows: usize,
    pub affected_rows: u64,
}

/// The sole planner-side entry point for SQL statement execution.
pub struct UnifiedPlanExecutor<'d, D: UnifiedPlanDriver> {
    driver: &'d D,
    last_stats: Option<UnifiedExecutionStats>,
}

impl<'d, D: UnifiedPlanDriver> UnifiedPlanExecutor<'d, D> {
    #[must_use]
    pub fn new(driver: &'d D) -> Self {
        Self {
            driver,
            last_stats: None,
        }
    }

    pub fn execute(&mut self, plan: &UnifiedPlan) -> Result<SQLResult, D::Error> {
        self.last_stats = None;
        let started = Instant::now();
        let result = self.driver.execute_plan(plan)?;
        self.last_stats = Some(UnifiedExecutionStats {
            plan_name: plan.name().to_string(),
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            result_rows: result.rows.len(),
            affected_rows: result.affected_rows,
        });
        Ok(result)
    }

    #[must_use]
    pub fn last_stats(&self) -> Option<&UnifiedExecutionStats> {
        self.last_stats.as_ref()
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
        assert!(matches!(block.compute, ComputePlan::Project { .. }));

        let window = one("SELECT row_number() OVER (ORDER BY a) AS n FROM t");
        let UnifiedPlan::Query(query) = window else {
            panic!("expected query plan");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("expected query block");
        };
        assert!(matches!(block.compute, ComputePlan::Window { .. }));
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
        assert!(matches!(block.source, SourcePlan::Subquery { .. }));
        assert_eq!(
            block.compute.projections()[0].expression.subqueries.len(),
            1
        );
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
        let CommandPlan::Update {
            ctes,
            source,
            expressions,
            ..
        } = update.as_ref()
        else {
            panic!("expected UPDATE plan");
        };
        assert_eq!(ctes.len(), 1);
        assert!(matches!(source.as_deref(), Some(SourcePlan::Table { .. })));
        assert_eq!(
            expressions
                .iter()
                .map(|expression| expression.subqueries.len())
                .sum::<usize>(),
            1
        );

        let merge = one("MERGE INTO target USING (SELECT id, v FROM source) AS s \
             ON target.id = s.id WHEN MATCHED THEN UPDATE SET v = s.v");
        let UnifiedPlan::Command(merge) = merge else {
            panic!("MERGE must be a command plan");
        };
        let CommandPlan::Merge { source, .. } = merge.as_ref() else {
            panic!("expected MERGE plan");
        };
        assert!(matches!(source.as_ref(), SourcePlan::Subquery { .. }));
    }
}
