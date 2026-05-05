//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Internal SQL AST. Lifts the relevant subset of the `libpg_query`
//! protobuf tree into a Rust enum the compiler walks. Statements not
//! yet supported parse cleanly but compile to
//! [`crate::SQLError::Unsupported`].

use serde::{Deserialize, Serialize};
use uqa_core::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ColumnType {
    Integer,
    Text,
    Real,
    /// `VECTOR(N)` columns store an `N`-dimensional `f32` embedding.
    Vector(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub ty: ColumnType,
    pub primary_key: bool,
    pub not_null: bool,
    /// `SERIAL` / `BIGSERIAL` columns auto-allocate from a per-table
    /// monotonic counter when the value is omitted from `INSERT`.
    #[serde(default)]
    pub auto_increment: bool,
}

#[derive(Debug, Clone)]
pub struct CreateTable {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    /// `CREATE TABLE IF NOT EXISTS` — silently ignore the statement
    /// when a table with this name already exists.
    pub if_not_exists: bool,
}

#[derive(Debug, Clone)]
pub struct CreateIndex {
    pub name: Option<String>,
    pub table: String,
    /// `gin`, `btree`, `ivf`, `rtree`, `hnsw`, ...
    pub access_method: String,
    pub columns: Vec<String>,
    /// `CREATE INDEX IF NOT EXISTS`.
    pub if_not_exists: bool,
    /// Storage parameters from `WITH (k = v, ...)`. Stored verbatim;
    /// known keys (`analyzer`, `lists`, `m`, `ef_construction`, ...)
    /// are interpreted by the engine.
    pub options: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct DropStmt {
    pub kind: DropKind,
    pub names: Vec<String>,
    pub if_exists: bool,
    pub cascade: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropKind {
    Table,
    Index,
    View,
    Schema,
}

#[derive(Debug, Clone)]
pub struct AlterTableStmt {
    pub table: String,
    pub if_exists: bool,
    pub action: AlterTableAction,
}

#[derive(Debug, Clone)]
pub enum AlterTableAction {
    AddColumn {
        column: ColumnDef,
        if_not_exists: bool,
    },
    DropColumn {
        name: String,
        if_exists: bool,
        cascade: bool,
    },
    RenameColumn {
        from: String,
        to: String,
    },
    RenameTable {
        to: String,
    },
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
    pub from: Option<FromClause>,
    pub r#where: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub order_by: Vec<OrderBy>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    /// Common table expressions defined with `WITH [RECURSIVE] ...`.
    pub with: Vec<CTE>,
    /// Optional set operation: `Some` for UNION / INTERSECT / EXCEPT,
    /// with the right-hand operand as a sub-select.
    pub set_op: Option<Box<SetOp>>,
    /// `SELECT DISTINCT` -- de-duplicate the final result rows. Set by
    /// the compiler whenever the parsed `distinct_clause` is non-empty.
    pub distinct: bool,
}

#[derive(Debug, Clone)]
pub struct CTE {
    pub name: String,
    pub recursive: bool,
    pub query: Box<SelectStmt>,
}

#[derive(Debug, Clone)]
pub struct SetOp {
    pub kind: SetOpKind,
    pub all: bool,
    pub right: SelectStmt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOpKind {
    Union,
    Intersect,
    Except,
}

#[derive(Debug, Clone)]
pub enum FromClause {
    /// `FROM <table> [AS <alias>]`.
    Table { name: String, alias: Option<String> },
    /// `FROM left <kind> right ON predicate`.
    Join {
        left: Box<FromClause>,
        right: Box<FromClause>,
        kind: JoinKind,
        on: Option<Expr>,
    },
}

impl FromClause {
    /// All table names referenced under this clause, in declaration
    /// order. Used by the compiler to resolve unqualified column refs.
    pub fn collect_tables(&self, out: &mut Vec<(String, Option<String>)>) {
        match self {
            FromClause::Table { name, alias } => out.push((name.clone(), alias.clone())),
            FromClause::Join { left, right, .. } => {
                left.collect_tables(out);
                right.collect_tables(out);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
    Cross,
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

#[derive(Debug, Clone)]
pub struct WindowSpec {
    pub partition_by: Vec<Expr>,
    pub order_by: Vec<OrderBy>,
}

/// Scalar expression nodes the compiler handles.
#[derive(Debug, Clone)]
pub enum Expr {
    Star,
    /// Unqualified column reference (`col`).
    Column(String),
    /// Qualified column reference (`table.col` or `alias.col`).
    QualifiedColumn {
        qualifier: String,
        column: String,
    },
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
    /// `lhs op rhs` — comparison or arithmetic.
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `NOT expr`.
    Not(Box<Expr>),
    /// `cond_1 AND cond_2 AND ...` (n-ary).
    And(Vec<Expr>),
    /// `cond_1 OR cond_2 OR ...` (n-ary).
    Or(Vec<Expr>),
    /// `expr IS NULL` / `expr IS NOT NULL`.
    IsNull {
        expr: Box<Expr>,
        negated: bool,
    },
    /// `expr BETWEEN low AND high`.
    Between {
        expr: Box<Expr>,
        low: Box<Expr>,
        high: Box<Expr>,
    },
    /// `expr IN (a, b, c)` literal list.
    InList {
        expr: Box<Expr>,
        list: Vec<Expr>,
        negated: bool,
    },
    /// `func(args) OVER (PARTITION BY ... ORDER BY ...)`.
    WindowCall {
        name: String,
        args: Vec<Expr>,
        spec: WindowSpec,
    },
    /// `CASE [base] WHEN cond THEN result ... [ELSE default] END`.
    /// `base` lifts simple-form `CASE expr WHEN val THEN ...` into an
    /// optional comparison anchor; searched-form `CASE WHEN cond ...`
    /// leaves it `None`.
    Case {
        base: Option<Box<Expr>>,
        when: Vec<(Expr, Expr)>,
        else_branch: Option<Box<Expr>>,
    },
    /// `CAST(expr AS type)`. The type name is preserved verbatim so
    /// the evaluator can apply the correct coercion.
    Cast {
        expr: Box<Expr>,
        ty: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Add,
    Subtract,
    Multiply,
    Divide,
}

/// `Expr` restricted to value-producing forms used by `INSERT` rows.
pub type ValueExpr = Expr;

#[derive(Debug, Clone)]
pub struct UpdateStmt {
    pub table: String,
    pub assignments: Vec<(String, Expr)>,
    pub r#where: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct DeleteStmt {
    pub table: String,
    pub r#where: Option<Expr>,
}

#[derive(Debug, Clone)]
pub enum Statement {
    CreateTable(CreateTable),
    CreateIndex(CreateIndex),
    Insert(InsertStmt),
    /// `SelectStmt` is the largest variant by far (CTEs + set-ops + n-ary
    /// expression trees), so we box it to keep the enum's stack footprint
    /// proportional to the smaller variants.
    Select(Box<SelectStmt>),
    Update(UpdateStmt),
    Delete(DeleteStmt),
    Drop(DropStmt),
    AlterTable(AlterTableStmt),
}
