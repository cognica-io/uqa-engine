use serde::{Deserialize, Serialize};
use uqa_core::Value;

use super::SelectStmt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Projection {
    pub expr: Expr,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBy {
    pub expr: Expr,
    pub descending: bool,
    /// `NULLS FIRST` / `NULLS LAST` placement. `None` means the
    /// SQL-standard default - `NULLS LAST` for ASC and `NULLS FIRST`
    /// for DESC. Mirrors `PostgreSQL` semantics.
    pub nulls: Option<NullsOrder>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NullsOrder {
    First,
    Last,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSpec {
    pub partition_by: Vec<Expr>,
    pub order_by: Vec<OrderBy>,
    /// `ROWS` / `RANGE` frame, or `None` when not specified (defaults
    /// to `RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW`).
    pub frame: Option<WindowFrame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowFrame {
    pub mode: FrameMode,
    pub start: FrameBound,
    pub end: FrameBound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameMode {
    Rows,
    Range,
    Groups,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FrameBound {
    UnboundedPreceding,
    UnboundedFollowing,
    CurrentRow,
    Preceding(Box<Expr>),
    Following(Box<Expr>),
}

/// Scalar expression nodes the compiler handles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Expr {
    Star,
    /// Unqualified column reference (`col`).
    Column(String),
    /// Qualified column reference (`table.col` or `alias.col`).
    QualifiedColumn {
        qualifier: String,
        column: String,
        #[serde(default)]
        key: String,
    },
    Literal(Value),
    /// A positional bind parameter (`$1`, `$2`, ...).
    Param(usize),
    /// `text_match(...)`, `knn_match(...)`, etc. - dispatched through
    /// the function registry.
    Func {
        name: String,
        args: Vec<Expr>,
        /// `func(DISTINCT expr)` - only meaningful for aggregate
        /// functions. Mirrors `PostgreSQL`'s `agg_distinct`.
        distinct: bool,
        /// `func(expr ORDER BY ...)` - only meaningful for ordered
        /// aggregates (`STRING_AGG`, `ARRAY_AGG`, `PERCENTILE_*`).
        order_by: Vec<OrderBy>,
        /// `func(...) FILTER (WHERE expr)` - aggregate-level row filter.
        filter: Option<Box<Expr>>,
    },
    /// `ARRAY[1.0, 2.0, ...]` literal - currently restricted to numeric
    /// elements (vectors).
    Array(Vec<Expr>),
    /// `lhs op rhs` - comparison or arithmetic.
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
    /// `(SELECT ...)` scalar subquery: yields a single row / single
    /// column value at evaluation time.
    ScalarSubquery(Box<SelectStmt>),
    /// `EXISTS (SELECT ...)` -- truthy when the body produces at
    /// least one row.
    Exists {
        body: Box<SelectStmt>,
        negated: bool,
    },
    /// `expr [NOT] IN (SELECT ...)` set membership against a
    /// subquery. Evaluator runs the body once per top-level
    /// expression and tests membership.
    InSubquery {
        expr: Box<Expr>,
        body: Box<SelectStmt>,
        negated: bool,
    },
}

impl Expr {
    pub fn qualified_column(qualifier: impl Into<String>, column: impl Into<String>) -> Self {
        let qualifier = qualifier.into();
        let column = column.into();
        let key = format!("{qualifier}.{column}");
        Self::QualifiedColumn {
            qualifier,
            column,
            key,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
