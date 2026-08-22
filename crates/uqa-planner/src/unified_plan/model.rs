//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Serializable relational, command, source, and scalar plan data model.

use super::{NullsOrder, ScalarExpr, SetOpKind};

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
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        with_ties: bool,
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub group_distinct: bool,
    pub having: Option<ScalarExpr>,
    pub order_by: Vec<OrderPlan>,
    pub limit: Option<ScalarExpr>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub with_ties: bool,
    pub offset: Option<ScalarExpr>,
    pub distinct: bool,
    pub distinct_on: Vec<ScalarExpr>,
    pub subqueries: Vec<QueryPlan>,
    pub access: AccessPathPlan,
    /// `FOR UPDATE` / `FOR SHARE` clauses belonging to this query block.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locking: Vec<uqa_sql::ast::LockingClause>,
}

/// Cross-paradigm access decision made after the relational and scalar
/// portions of a query block have both been lowered.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AccessPathPlan {
    /// Ordinary row-source execution.
    Row,
    /// Use the shared document-support/operator algebra for the block predicate.
    OperatorTree {
        /// The relational ORDER BY/OFFSET/LIMIT can be pushed into the
        /// retrieval function before row materialization.
        score_limit_pushdown: bool,
    },
    /// Split a mixed predicate into posting-list candidates followed by
    /// row-level residual evaluation.
    Hybrid,
}

/// Physical strategy selected for a relational join.
///
/// `Auto` is used for an unreordered SQL join and lets physical lowering pick
/// hash execution for a splittable equality predicate or nested-loop execution
/// otherwise. `Hash` is an optimizer commitment produced by DPccp and must be
/// executable; physical lowering reports an internal planning error if that
/// invariant is violated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum JoinExecutionStrategy {
    #[default]
    Auto,
    Hash,
}

/// The row-producing source below a query block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum SourcePlan {
    Table {
        name: String,
        #[serde(default)]
        qualifier: String,
        alias: Option<String>,
    },
    Join {
        left: Box<SourcePlan>,
        right: Box<SourcePlan>,
        kind: uqa_sql::ast::JoinKind,
        on: Option<ScalarExpr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        using: Option<uqa_sql::ast::JoinUsing>,
        #[serde(default)]
        natural: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        column_aliases: Vec<String>,
        lateral: bool,
        #[serde(default)]
        strategy: JoinExecutionStrategy,
    },
    Values {
        rows: Vec<Vec<ScalarExpr>>,
        alias: Option<String>,
        column_aliases: Vec<String>,
    },
    Function {
        name: String,
        #[serde(default)]
        output_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relation: Option<String>,
        args: Vec<ScalarExpr>,
        alias: Option<String>,
        column_aliases: Vec<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ordinality: bool,
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
    pub target_qualifier: String,
    pub columns: Vec<String>,
    pub ctes: Vec<CtePlan>,
    pub rows: Vec<Vec<ScalarExpr>>,
    pub source: Option<Box<QueryPlan>>,
    pub on_conflict: Option<ConflictPlan>,
    pub returning: Vec<ProjectionPlan>,
    pub returning_aliases: uqa_sql::ast::ReturningAliases,
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
    pub target_qualifier: String,
    pub assignments: Vec<AssignmentPlan>,
    pub predicate: Option<ScalarExpr>,
    pub ctes: Vec<CtePlan>,
    pub source: Option<Box<SourcePlan>>,
    pub returning: Vec<ProjectionPlan>,
    pub returning_aliases: uqa_sql::ast::ReturningAliases,
    pub subqueries: Vec<QueryPlan>,
}

#[derive(Debug, Clone)]
pub struct DeletePlan {
    pub table: String,
    pub target_qualifier: String,
    pub predicate: Option<ScalarExpr>,
    pub ctes: Vec<CtePlan>,
    pub source: Option<Box<SourcePlan>>,
    pub returning: Vec<ProjectionPlan>,
    pub returning_aliases: uqa_sql::ast::ReturningAliases,
    pub subqueries: Vec<QueryPlan>,
}

#[derive(Debug, Clone)]
pub struct MergePlan {
    pub target: String,
    pub target_qualifier: String,
    pub target_alias: Option<String>,
    pub source: Box<SourcePlan>,
    pub join_condition: ScalarExpr,
    pub when_clauses: Vec<MergeWhenPlan>,
    pub returning: Vec<ProjectionPlan>,
    pub returning_aliases: uqa_sql::ast::ReturningAliases,
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
    Load {
        library: String,
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
        column_names: Vec<String>,
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

pub(super) struct NoRegisteredAggregates;

impl AggregateClassifier for NoRegisteredAggregates {
    fn is_registered_aggregate(&self, _name: &str) -> bool {
        false
    }
}
