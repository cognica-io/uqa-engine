//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! AST-independent scalar physical IR shared by the planner and executors.

mod call_arguments;
mod context;
mod evaluator;
mod subquery;
mod traversal;

use uqa_core::Value;
use uqa_sql::ast::{BinaryOp, FrameMode, FunctionBinding, InternalColumnRef, NullsOrder};

/// Index into the query children owned by the enclosing expression plan.
pub type SubqueryId = usize;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ScalarExpr {
    Star,
    QualifiedStar(String),
    Default,
    Column(String),
    /// Logical position in an already-bound physical row schema. This variant is introduced only after relational binding so duplicate SQL labels remain independently addressable.
    Position(usize),
    /// Structural executor-only attribute, resolved independently of SQL relation and column names.
    InternalColumn(InternalColumnRef),
    QualifiedColumn {
        qualifier: String,
        column: String,
    },
    Literal(Value),
    Param(usize),
    Func {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        binding: Option<FunctionBinding>,
        args: Vec<Self>,
        distinct: bool,
        order_by: Vec<ScalarOrder>,
        filter: Option<Box<Self>>,
    },
    Array(Vec<Self>),
    Row(Vec<Self>),
    Binary {
        op: BinaryOp,
        lhs: Box<Self>,
        rhs: Box<Self>,
    },
    UnaryMinus(Box<Self>),
    Not(Box<Self>),
    And(Vec<Self>),
    Or(Vec<Self>),
    IsNull {
        expr: Box<Self>,
        negated: bool,
    },
    Between {
        expr: Box<Self>,
        low: Box<Self>,
        high: Box<Self>,
    },
    InList {
        expr: Box<Self>,
        list: Vec<Self>,
        negated: bool,
    },
    WindowCall {
        name: String,
        args: Vec<Self>,
        spec: ScalarWindowSpec,
    },
    Case {
        base: Option<Box<Self>>,
        when: Vec<(Self, Self)>,
        else_branch: Option<Box<Self>>,
    },
    Cast {
        expr: Box<Self>,
        ty: String,
    },
    ScalarSubquery(SubqueryId),
    Exists {
        subquery: SubqueryId,
        negated: bool,
    },
    InSubquery {
        expr: Box<Self>,
        subquery: SubqueryId,
        negated: bool,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScalarOrder {
    pub expr: ScalarExpr,
    pub descending: bool,
    pub nulls: Option<NullsOrder>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScalarWindowSpec {
    pub partition_by: Vec<ScalarExpr>,
    pub order_by: Vec<ScalarOrder>,
    pub frame: Option<ScalarWindowFrame>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScalarWindowFrame {
    pub mode: FrameMode,
    pub start: ScalarFrameBound,
    pub end: ScalarFrameBound,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ScalarFrameBound {
    UnboundedPreceding,
    UnboundedFollowing,
    CurrentRow,
    Preceding(Box<ScalarExpr>),
    Following(Box<ScalarExpr>),
}

impl ScalarExpr {
    #[must_use]
    pub fn qualified_column(qualifier: impl Into<String>, column: impl Into<String>) -> Self {
        Self::QualifiedColumn {
            qualifier: qualifier.into(),
            column: column.into(),
        }
    }
}

pub use call_arguments::{
    eval_call_arguments, scalar_call_argument, scalar_call_arguments,
    validate_scalar_call_arguments, ScalarCallArgument,
};
pub use context::ScalarEvalContext;
pub use evaluator::eval_scalar;
pub(crate) use evaluator::scalar_integer_binary_width;
pub use subquery::{ScalarSubqueryRunner, SubqueryResult};

#[cfg(test)]
mod tests;
