//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cypher AST. Mirrors the openCypher subset implemented by the
//! UQA Cypher subset: `MATCH`, `OPTIONAL MATCH`, `CREATE`, `MERGE`,
//! `SET`, `DELETE`, `DETACH DELETE`, `RETURN`, `WITH`, `WHERE`,
//! `ORDER BY`, `SKIP`, `LIMIT`, `UNWIND`.

use std::collections::BTreeMap;

use uqa_core::Value;

// -- Expressions ----------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct PropertyAccess {
    pub variable: String,
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parameter {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Literal {
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variable {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub args: Vec<CypherExpr>,
    pub distinct: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BinaryOp {
    pub op: String,
    pub left: Box<CypherExpr>,
    pub right: Box<CypherExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnaryOp {
    pub op: String,
    pub operand: Box<CypherExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListIndex {
    pub expr: Box<CypherExpr>,
    pub index: Box<CypherExpr>,
}

/// `expr[start..end]` slice; either bound may be omitted. Slices are
/// end-exclusive and support negative offsets.
#[derive(Debug, Clone, PartialEq)]
pub struct ListSlice {
    pub expr: Box<CypherExpr>,
    pub start: Option<Box<CypherExpr>>,
    pub end: Option<Box<CypherExpr>>,
}

/// `[variable IN list WHERE filter | map]` list comprehension.
#[derive(Debug, Clone, PartialEq)]
pub struct ListComprehension {
    pub variable: String,
    pub list_expr: Box<CypherExpr>,
    pub filter: Option<Box<CypherExpr>>,
    pub map_expr: Option<Box<CypherExpr>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InList {
    pub expr: Box<CypherExpr>,
    pub list_expr: Box<CypherExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IsNull {
    pub expr: Box<CypherExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IsNotNull {
    pub expr: Box<CypherExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaseExpr {
    pub operand: Option<Box<CypherExpr>>,
    pub whens: Vec<(CypherExpr, CypherExpr)>,
    pub else_expr: Option<Box<CypherExpr>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListLiteral {
    pub elements: Vec<CypherExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapLiteral {
    pub pairs: Vec<(String, CypherExpr)>,
}

/// Top-level expression node.
#[derive(Debug, Clone, PartialEq)]
pub enum CypherExpr {
    PropertyAccess(PropertyAccess),
    Parameter(Parameter),
    Literal(Literal),
    Variable(Variable),
    FunctionCall(FunctionCall),
    BinaryOp(BinaryOp),
    UnaryOp(UnaryOp),
    ListIndex(ListIndex),
    ListSlice(ListSlice),
    ListComprehension(ListComprehension),
    InList(InList),
    IsNull(IsNull),
    IsNotNull(IsNotNull),
    CaseExpr(CaseExpr),
    ListLiteral(ListLiteral),
    MapLiteral(MapLiteral),
    /// `exists((a)-[:R]->(b))` pattern predicate.
    ExistsPattern(PathPattern),
}

// -- Patterns ------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct NodePattern {
    pub variable: Option<String>,
    pub labels: Vec<String>,
    pub properties: Option<BTreeMap<String, CypherExpr>>,
}

/// Direction of a relationship pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelDirection {
    /// `-[...]->`
    Right,
    /// `<-[...]-`
    Left,
    /// `-[...]-`
    Both,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RelPattern {
    pub variable: Option<String>,
    pub types: Vec<String>,
    pub properties: Option<BTreeMap<String, CypherExpr>>,
    pub direction: RelDirection,
    /// `None` = exactly 1; `Some` = variable-length lower bound
    /// (`*<min>..<max>`).
    pub min_hops: Option<u32>,
    /// `None` = unbounded (or exactly 1 if `min_hops` is also `None`).
    pub max_hops: Option<u32>,
}

/// One element in a path: a node or a relationship.
#[derive(Debug, Clone, PartialEq)]
pub enum PathElement {
    Node(NodePattern),
    Rel(RelPattern),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathPattern {
    /// Path variable when the pattern is `p = (...)-[...]-(...)`.
    pub variable: Option<String>,
    pub elements: Vec<PathElement>,
}

// -- Clauses --------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct MatchClause {
    pub patterns: Vec<PathPattern>,
    pub r#where: Option<CypherExpr>,
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateClause {
    pub patterns: Vec<PathPattern>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergeClause {
    pub pattern: PathPattern,
    pub on_create_set: Option<Vec<SetItem>>,
    pub on_match_set: Option<Vec<SetItem>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOperator {
    /// `=`
    Assign,
    /// `+=`
    Update,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SetItem {
    pub target: CypherExpr,
    pub value: CypherExpr,
    pub operator: SetOperator,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SetClause {
    pub items: Vec<SetItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeleteClause {
    pub expressions: Vec<CypherExpr>,
    pub detach: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnItem {
    pub expr: CypherExpr,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderByItem {
    pub expr: CypherExpr,
    pub ascending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnClause {
    pub items: Vec<ReturnItem>,
    pub distinct: bool,
    pub order_by: Option<Vec<OrderByItem>>,
    pub skip: Option<CypherExpr>,
    pub limit: Option<CypherExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WithClause {
    pub items: Vec<ReturnItem>,
    pub distinct: bool,
    pub order_by: Option<Vec<OrderByItem>>,
    pub skip: Option<CypherExpr>,
    pub limit: Option<CypherExpr>,
    pub r#where: Option<CypherExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnwindClause {
    pub expr: CypherExpr,
    pub variable: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CypherClause {
    Match(MatchClause),
    Create(CreateClause),
    Merge(MergeClause),
    Set(SetClause),
    Delete(DeleteClause),
    Return(ReturnClause),
    With(WithClause),
    Unwind(UnwindClause),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CypherQuery {
    pub clauses: Vec<CypherClause>,
}
