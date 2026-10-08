//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use uqa_core::Value;

use super::{FunctionBinding, SelectStmt};

/// Query-local identity for an executor-only row source. Parser-produced SQL never contains this identity, so internal row carriers cannot collide with user relation aliases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[doc(hidden)]
pub struct InternalRelationId(u64);

impl InternalRelationId {
    /// Allocate an opaque relation identity for an engine-injected row source.
    #[must_use]
    pub fn allocate() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = uqa_core::atomic::try_update_u64(
            &NEXT_ID,
            Ordering::Relaxed,
            Ordering::Relaxed,
            |current| current.checked_add(1),
        )
        .expect("internal relation identity space exhausted");
        Self(id)
    }

    /// Address one zero-based attribute of this internal relation.
    #[must_use]
    pub fn column(self, attribute: usize) -> InternalColumnRef {
        InternalColumnRef {
            relation: self,
            attribute: u32::try_from(attribute).expect("internal relation attribute exceeds u32"),
        }
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

/// Structural reference to an executor-only relation attribute. This is the UQA analogue of PostgreSQL's `Var(varno, varattno)` identity: it is never resolved through SQL text names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[doc(hidden)]
pub struct InternalColumnRef {
    relation: InternalRelationId,
    attribute: u32,
}

impl InternalColumnRef {
    #[must_use]
    pub const fn relation(self) -> InternalRelationId {
        self.relation
    }

    #[must_use]
    pub const fn attribute(self) -> usize {
        self.attribute as usize
    }

    #[must_use]
    pub const fn from_raw(relation: u64, attribute: u32) -> Self {
        Self {
            relation: InternalRelationId::from_raw(relation),
            attribute,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    pub expr: Expr,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// One query-local window definition. The specification owns only its written clauses; inheritance is resolved through an earlier named definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowDefinition<S = WindowSpec> {
    pub name: Option<String>,
    pub inherited: Option<usize>,
    pub spec: S,
}

/// Compiler-only equality key for a raw window declaration; not part of stored SQL.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowDefinitionSyntax(pub(crate) serde_json::Value);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowSpec {
    /// Raw syntax survives only until the compiler selects its query-local definition.
    #[serde(skip)]
    pub raw_definition: Option<WindowDefinitionSyntax>,
    /// Canonical definition in the enclosing query block. Older stored inline specifications have no slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<usize>,
    /// Named window referenced by this specification while the SQL compiler resolves a `WINDOW` clause. Compiler-produced plans clear this field before lowering into the unified scalar IR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<WindowReference>,
    pub partition_by: Vec<Expr>,
    pub order_by: Vec<OrderBy>,
    /// `ROWS` / `RANGE` frame, or `None` when not specified (defaults
    /// to `RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW`).
    pub frame: Option<WindowFrame>,
}

impl WindowSpec {
    /// Scalar roots owned by this specification, excluding separately stored inherited clauses.
    pub fn expressions_mut(&mut self) -> impl Iterator<Item = &mut Expr> {
        self.partition_by
            .iter_mut()
            .chain(self.order_by.iter_mut().map(|order| &mut order.expr))
            .chain(
                self.frame
                    .iter_mut()
                    .flat_map(|frame| [&mut frame.start, &mut frame.end])
                    .filter_map(|bound| match bound {
                        FrameBound::Preceding(value) | FrameBound::Following(value) => {
                            Some(value.as_mut())
                        }
                        _ => None,
                    }),
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowReference {
    pub name: String,
    pub kind: WindowReferenceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowReferenceKind {
    /// `OVER window_name` uses the named definition directly, including its frame.
    Direct,
    /// `OVER (window_name ...)` or `WINDOW child AS (parent ...)` copies and may extend a frameless definition.
    Copy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowFrame {
    pub mode: FrameMode,
    pub start: FrameBound,
    pub end: FrameBound,
    /// Whether the frame was written `BETWEEN start AND end`. A frame that names only its start, such as `ROWS UNBOUNDED PRECEDING`, ends at the current row, and the definition is deparsed as it was written.
    #[serde(default = "super::default_true")]
    pub between: bool,
    #[serde(default, skip_serializing_if = "FrameExclusion::is_no_others")]
    pub exclusion: FrameExclusion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameMode {
    Rows,
    Range,
    Groups,
}

/// The written function-call form, retained independently of overload binding. The durable `order_syntax` field keeps its original name for stored-expression compatibility.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionCallSyntax {
    #[default]
    Legacy,
    Ordinary,
    WithinGroup,
    Extract,
}

/// The original public name, preserved for callers and stored ordering metadata.
pub type FunctionOrderSyntax = FunctionCallSyntax;

impl FunctionCallSyntax {
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy)
    }
}

/// The aggregate modifiers written on a window call, which `ParseFuncOrColumn` rejects after it has resolved the function. The arguments of a call written with `WITHIN GROUP` include its ordering expressions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowCallModifiers {
    pub distinct: bool,
    /// An aggregate `ORDER BY` inside the argument list.
    pub ordered: bool,
    pub within_group: bool,
}

impl WindowCallModifiers {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.distinct && !self.ordered && !self.within_group
    }
}

/// The frame exclusion clause: the rows of the frame that a window function or aggregate does not see.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameExclusion {
    #[default]
    NoOthers,
    CurrentRow,
    /// The current row and its peers.
    Group,
    /// The peers of the current row, but not the current row itself.
    Ties,
}

impl FrameExclusion {
    #[must_use]
    pub const fn is_no_others(&self) -> bool {
        matches!(self, Self::NoOthers)
    }

    /// The clause as `pg_get_viewdef` spells it, or `None` for `EXCLUDE NO OTHERS`, which it omits.
    #[must_use]
    pub const fn sql(self) -> Option<&'static str> {
        match self {
            Self::NoOthers => None,
            Self::CurrentRow => Some("EXCLUDE CURRENT ROW"),
            Self::Group => Some("EXCLUDE GROUP"),
            Self::Ties => Some("EXCLUDE TIES"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FrameBound {
    UnboundedPreceding,
    UnboundedFollowing,
    CurrentRow,
    Preceding(Box<Expr>),
    Following(Box<Expr>),
}

/// Scalar expression nodes the compiler handles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    Star,
    /// Relation-qualified wildcard projection (`table.*` or `alias.*`).
    QualifiedStar(String),
    /// `DEFAULT` in an INSERT/UPDATE assignment is a mutation marker resolved against the target column before expression evaluation.
    Default,
    /// Unqualified column reference (`col`).
    Column(String),
    /// Qualified column reference (`table.col` or `alias.col`).
    QualifiedColumn {
        qualifier: String,
        column: String,
    },
    /// Structural column reference emitted internally rather than by SQL parsing; SQL name binding must not rewrite it.
    #[doc(hidden)]
    InternalColumn(InternalColumnRef),
    Literal(Value),
    /// An already-coerced runtime datum with its declared SQL type. Variable binding emits this leaf so reading a domain value does not repeat its constraints.
    #[doc(hidden)]
    TypedLiteral {
        value: Value,
        ty: String,
        /// Original composite input retained across non-invertible descriptor changes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        composite_source: Option<Box<crate::expr::composites::CompositeConstantSource>>,
    },
    /// A positional bind parameter (`$1`, `$2`, ...).
    Param(usize),
    /// Function call dispatched through the function registry.
    Func {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        binding: Option<FunctionBinding>,
        args: Vec<Expr>,
        /// `func(DISTINCT expr)` - only meaningful for aggregate
        /// functions. Mirrors `PostgreSQL`'s `agg_distinct`.
        distinct: bool,
        /// Ordering expressions from either `func(expr ORDER BY ...)` or `func(expr) WITHIN GROUP (ORDER BY ...)`; `order_syntax` preserves their written location.
        order_by: Vec<OrderBy>,
        #[serde(default, skip_serializing_if = "FunctionOrderSyntax::is_legacy")]
        order_syntax: FunctionOrderSyntax,
        /// `func(...) FILTER (WHERE expr)` - aggregate-level row filter.
        filter: Option<Box<Expr>>,
    },
    /// SQL array constructor.
    Array(Vec<Expr>),
    /// Anonymous SQL row constructor (`ROW(...)` or `(a, b)`).
    Row(Vec<Expr>),
    /// A stored typed row with creation-time attribute positions and already selected field conversions.
    CompositeRow {
        items: Vec<Expr>,
        binding: super::CompositeRowBinding,
    },
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
    /// `func(args) [FILTER (WHERE condition)] OVER (PARTITION BY ... ORDER BY ...)`. Only aggregates accept `FILTER`.
    WindowCall {
        name: String,
        args: Vec<Expr>,
        spec: Box<WindowSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<Box<Expr>>,
        #[serde(default, skip_serializing_if = "WindowCallModifiers::is_empty")]
        modifiers: WindowCallModifiers,
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
        /// Set by analysis for an implicit coercion retained in stored syntax.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        implicit: bool,
        expr: Box<Expr>,
        ty: String,
    },
    /// `(SELECT ...)` query expression: ordinary scalar consumers select one
    /// column; a multiple-column SET target consumes the positional result.
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

    /// Upgrade compiler-owned function markers deserialized from catalogs
    /// written by releases through 0.1.6.
    #[doc(hidden)]
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive AST migration preserves every serialized variant"
    )]
    pub fn upgrade_legacy_serialized_dispatches(&mut self) -> bool {
        let mut changed = false;
        match self {
            Self::Func {
                name,
                binding,
                args,
                order_by,
                filter,
                ..
            } => {
                for argument in args {
                    changed |= argument.upgrade_legacy_serialized_dispatches();
                }
                for order in order_by {
                    changed |= order.expr.upgrade_legacy_serialized_dispatches();
                }
                if let Some(filter) = filter {
                    changed |= filter.upgrade_legacy_serialized_dispatches();
                }
                changed |=
                    super::FunctionBinding::upgrade_legacy_serialized_dispatch(name, binding);
            }
            Self::Array(items)
            | Self::Row(items)
            | Self::CompositeRow { items, .. }
            | Self::And(items)
            | Self::Or(items) => {
                for item in items {
                    changed |= item.upgrade_legacy_serialized_dispatches();
                }
            }
            Self::Binary { lhs, rhs, .. } => {
                changed |= lhs.upgrade_legacy_serialized_dispatches();
                changed |= rhs.upgrade_legacy_serialized_dispatches();
            }
            Self::UnaryMinus(inner)
            | Self::Not(inner)
            | Self::IsNull { expr: inner, .. }
            | Self::Cast { expr: inner, .. } => {
                changed |= inner.upgrade_legacy_serialized_dispatches();
            }
            Self::Between { expr, low, high } => {
                changed |= expr.upgrade_legacy_serialized_dispatches();
                changed |= low.upgrade_legacy_serialized_dispatches();
                changed |= high.upgrade_legacy_serialized_dispatches();
            }
            Self::InList { expr, list, .. } => {
                changed |= expr.upgrade_legacy_serialized_dispatches();
                for item in list {
                    changed |= item.upgrade_legacy_serialized_dispatches();
                }
            }
            Self::WindowCall {
                args, spec, filter, ..
            } => {
                for argument in args {
                    changed |= argument.upgrade_legacy_serialized_dispatches();
                }
                if let Some(filter) = filter {
                    changed |= filter.upgrade_legacy_serialized_dispatches();
                }
                for partition in &mut spec.partition_by {
                    changed |= partition.upgrade_legacy_serialized_dispatches();
                }
                for order in &mut spec.order_by {
                    changed |= order.expr.upgrade_legacy_serialized_dispatches();
                }
                if let Some(frame) = &mut spec.frame {
                    for bound in [&mut frame.start, &mut frame.end] {
                        match bound {
                            FrameBound::Preceding(expression)
                            | FrameBound::Following(expression) => {
                                changed |= expression.upgrade_legacy_serialized_dispatches();
                            }
                            FrameBound::UnboundedPreceding
                            | FrameBound::UnboundedFollowing
                            | FrameBound::CurrentRow => {}
                        }
                    }
                }
            }
            Self::Case {
                base,
                when,
                else_branch,
            } => {
                if let Some(base) = base {
                    changed |= base.upgrade_legacy_serialized_dispatches();
                }
                for (condition, result) in when {
                    changed |= condition.upgrade_legacy_serialized_dispatches();
                    changed |= result.upgrade_legacy_serialized_dispatches();
                }
                if let Some(branch) = else_branch {
                    changed |= branch.upgrade_legacy_serialized_dispatches();
                }
            }
            Self::InSubquery { expr, body, .. } => {
                changed |= expr.upgrade_legacy_serialized_dispatches();
                changed |= body.upgrade_legacy_serialized_dispatches();
            }
            Self::ScalarSubquery(body) | Self::Exists { body, .. } => {
                changed |= body.upgrade_legacy_serialized_dispatches();
            }
            Self::Default
            | Self::Star
            | Self::QualifiedStar(_)
            | Self::Column(_)
            | Self::QualifiedColumn { .. }
            | Self::InternalColumn(_)
            | Self::Literal(_)
            | Self::TypedLiteral { .. }
            | Self::Param(_) => {}
        }
        changed
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
                Self::Func {
                    name,
                    args,
                    binding,
                    ..
                } if crate::expr::bound_scalar_function_strictness(
                    name,
                    binding.as_ref(),
                    args.len(),
                )
                .is_none()
            )
        })
    }

    /// Whether `hit` matches this node or any scalar node below it. Subquery bodies are opaque because they own independent query trees.
    #[must_use]
    pub fn any_node(&self, hit: &dyn Fn(&Self) -> bool) -> bool {
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
            Self::Array(items)
            | Self::Row(items)
            | Self::CompositeRow { items, .. }
            | Self::And(items)
            | Self::Or(items) => items.iter().any(|item| item.any_node(hit)),
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
            | Self::InternalColumn(_)
            | Self::Literal(_)
            | Self::TypedLiteral { .. }
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
