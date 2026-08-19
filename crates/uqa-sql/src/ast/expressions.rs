//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};
use uqa_core::Value;

use super::{FunctionBinding, SelectStmt};

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
    /// Relation-qualified wildcard projection (`table.*` or `alias.*`).
    QualifiedStar(String),
    /// `DEFAULT` in an INSERT/UPDATE assignment. This is a mutation marker,
    /// not a scalar value, and must be resolved against the target column
    /// before expression evaluation.
    Default,
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
    /// `text_match(...)`, `knn_match(...)`, etc. - dispatched through
    /// the function registry.
    Func {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        binding: Option<FunctionBinding>,
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
    /// Anonymous SQL row constructor (`ROW(...)` or `(a, b)`).
    Row(Vec<Expr>),
    /// `lhs op rhs` - comparison or arithmetic.
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `PostgreSQL` prefix `-`, kept distinct from binary subtraction so the
    /// operand's declared numeric width and overflow behavior survive lowering.
    UnaryMinus(Box<Expr>),
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
        Self::QualifiedColumn {
            qualifier: qualifier.into(),
            column: column.into(),
        }
    }

    /// True when this expression tree contains a window function call.
    #[must_use]
    pub fn contains_window(&self) -> bool {
        self.any_node(&|node| matches!(node, Self::WindowCall { .. }))
    }

    /// True when this expression tree contains a built-in aggregate call.
    #[must_use]
    pub fn contains_aggregate(&self) -> bool {
        self.any_node(
            &|node| matches!(node, Self::Func { name, .. } if is_builtin_aggregate_function(name)),
        )
    }

    /// True when this expression contains a column whose owning relation can only be determined after catalog schemas have been bound.
    #[must_use]
    pub fn contains_unqualified_column(&self) -> bool {
        self.any_node(&|node| matches!(node, Self::Column(_)))
    }

    /// True when this expression contains a function whose strictness cannot be decided without an engine catalog.
    #[must_use]
    pub fn contains_function_with_unknown_strictness(&self) -> bool {
        self.any_node(&|node| {
            matches!(
                node,
                Self::Func { name, args, .. }
                    if crate::expr::builtin_scalar_function_strictness(name, args.len()).is_none()
            )
        })
    }

    /// Whether `hit` matches this node or any node below it. Subquery bodies are opaque: `ScalarSubquery` and `Exists` own their expression trees.
    fn any_node(&self, hit: &dyn Fn(&Self) -> bool) -> bool {
        if hit(self) {
            return true;
        }
        match self {
            Self::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                args.iter().any(|arg| arg.any_node(hit))
                    || order_by.iter().any(|order| order.expr.any_node(hit))
                    || filter.as_deref().is_some_and(|filter| filter.any_node(hit))
            }
            Self::Array(items) | Self::Row(items) | Self::And(items) | Self::Or(items) => {
                items.iter().any(|item| item.any_node(hit))
            }
            Self::UnaryMinus(expr) | Self::Not(expr) | Self::Cast { expr, .. } => {
                expr.any_node(hit)
            }
            Self::Binary { lhs, rhs, .. } => lhs.any_node(hit) || rhs.any_node(hit),
            Self::IsNull { expr, .. } | Self::InSubquery { expr, .. } => expr.any_node(hit),
            Self::Between { expr, low, high } => {
                expr.any_node(hit) || low.any_node(hit) || high.any_node(hit)
            }
            Self::InList { expr, list, .. } => {
                expr.any_node(hit) || list.iter().any(|item| item.any_node(hit))
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                base.as_deref().is_some_and(|base| base.any_node(hit))
                    || when
                        .iter()
                        .any(|(condition, result)| condition.any_node(hit) || result.any_node(hit))
                    || else_branch
                        .as_deref()
                        .is_some_and(|branch| branch.any_node(hit))
            }
            Self::WindowCall { .. }
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Default
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
            | Self::Literal(_)
            | Self::Param(_)
            | Self::ScalarSubquery(_)
            | Self::Exists { .. } => false,
        }
    }
}

/// Return whether `name` is a built-in aggregate understood by the planner.
#[must_use]
pub fn is_builtin_aggregate_function(name: &str) -> bool {
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
