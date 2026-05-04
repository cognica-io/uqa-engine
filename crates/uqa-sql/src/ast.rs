//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Internal SQL AST. Lifts the relevant subset of the `libpg_query`
//! protobuf tree into a Rust enum the compiler walks. Statements not
//! yet supported parse cleanly but compile to
//! [`crate::SqlError::Unsupported`].

use uqa_core::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum ColumnType {
    Integer,
    Text,
    Real,
    /// `VECTOR(N)` columns store an `N`-dimensional `f32` embedding.
    Vector(u32),
}

#[derive(Debug, Clone)]
pub struct ColumnDef {
    pub name: String,
    pub ty: ColumnType,
    pub primary_key: bool,
    pub not_null: bool,
}

#[derive(Debug, Clone)]
pub struct CreateTable {
    pub name: String,
    pub columns: Vec<ColumnDef>,
}

#[derive(Debug, Clone)]
pub struct CreateIndex {
    pub name: Option<String>,
    pub table: String,
    /// `gin`, `btree`, `ivf`, `rtree`, ...
    pub access_method: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct InsertStmt {
    pub table: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<ValueExpr>>,
}

#[derive(Debug, Clone)]
pub struct SelectStmt {
    pub projections: Vec<Projection>,
    pub from: Option<String>,
    pub r#where: Option<Expr>,
    pub order_by: Vec<OrderBy>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Projection {
    pub expr: Expr,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OrderBy {
    pub expr: Expr,
    pub descending: bool,
}

/// Scalar expression nodes the compiler handles.
#[derive(Debug, Clone)]
pub enum Expr {
    Star,
    Column(String),
    Literal(Value),
    /// A positional bind parameter (`$1`, `$2`, ...).
    Param(usize),
    /// `text_match(...)`, `knn_match(...)`, etc. — dispatched through
    /// the function registry.
    Func {
        name: String,
        args: Vec<Expr>,
    },
    /// `ARRAY[1.0, 2.0, ...]` literal — currently restricted to numeric
    /// elements (vectors).
    Array(Vec<Expr>),
}

/// `Expr` restricted to value-producing forms used by `INSERT` rows.
pub type ValueExpr = Expr;

#[derive(Debug, Clone)]
pub enum Statement {
    CreateTable(CreateTable),
    CreateIndex(CreateIndex),
    Insert(InsertStmt),
    Select(SelectStmt),
}
