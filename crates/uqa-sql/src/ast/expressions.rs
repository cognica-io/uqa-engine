//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use uqa_core::Value;

use super::{
    FromClause, FunctionBinding, FunctionBody, MergeWhen, OnConflictAction, SelectStmt, Statement,
    CTE,
};

/// Query-local identity for an executor-only row source. Parser-produced SQL
/// never contains this identity, so internal row carriers cannot collide with
/// user relation aliases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[doc(hidden)]
pub struct InternalRelationId(u64);

impl InternalRelationId {
    /// Allocate an opaque relation identity for an engine-injected row source.
    #[must_use]
    pub fn allocate() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
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

/// Structural reference to an executor-only relation attribute. This is the
/// UQA analogue of PostgreSQL's `Var(varno, varattno)` identity: it is never
/// resolved through SQL text names.
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowSpec {
    /// Named window referenced by this specification while the SQL compiler resolves a `WINDOW` clause. Compiler-produced plans clear this field before lowering into the unified scalar IR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<WindowReference>,
    pub partition_by: Vec<Expr>,
    pub order_by: Vec<OrderBy>,
    /// `ROWS` / `RANGE` frame, or `None` when not specified (defaults
    /// to `RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW`).
    pub frame: Option<WindowFrame>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameMode {
    Rows,
    Range,
    Groups,
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
    /// Engine-injected structural column reference. SQL parsing never emits
    /// this variant and SQL name binding must not rewrite it.
    #[doc(hidden)]
    InternalColumn(InternalColumnRef),
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
            Self::Array(items) | Self::Row(items) | Self::And(items) | Self::Or(items) => {
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
            Self::WindowCall { args, spec, .. } => {
                for argument in args {
                    changed |= argument.upgrade_legacy_serialized_dispatches();
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
            | Self::InternalColumn(_)
            | Self::Literal(_)
            | Self::Param(_)
            | Self::ScalarSubquery(_)
            | Self::Exists { .. } => false,
        }
    }
}

fn upgrade_exprs(expressions: &mut [Expr]) -> bool {
    expressions.iter_mut().fold(false, |changed, expression| {
        expression.upgrade_legacy_serialized_dispatches() | changed
    })
}

fn upgrade_rows(rows: &mut [Vec<Expr>]) -> bool {
    rows.iter_mut()
        .fold(false, |changed, row| upgrade_exprs(row) | changed)
}

fn upgrade_optional(expression: &mut Option<Expr>) -> bool {
    expression
        .as_mut()
        .is_some_and(Expr::upgrade_legacy_serialized_dispatches)
}

fn upgrade_projections(projections: &mut [Projection]) -> bool {
    projections.iter_mut().fold(false, |changed, projection| {
        projection.expr.upgrade_legacy_serialized_dispatches() | changed
    })
}

fn upgrade_assignments(assignments: &mut [(String, Expr)]) -> bool {
    assignments
        .iter_mut()
        .fold(false, |changed, (_, expression)| {
            expression.upgrade_legacy_serialized_dispatches() | changed
        })
}

fn upgrade_ctes(ctes: &mut [CTE]) -> bool {
    ctes.iter_mut().fold(false, |mut changed, cte| {
        if let Some(cycle) = &mut cte.cycle {
            changed |= cycle.mark_value.upgrade_legacy_serialized_dispatches();
            changed |= cycle.mark_default.upgrade_legacy_serialized_dispatches();
        }
        changed | cte.query.upgrade_legacy_serialized_dispatches()
    })
}

impl FromClause {
    fn upgrade_legacy_serialized_dispatches(&mut self) -> bool {
        match self {
            Self::Table { .. } => false,
            Self::Join {
                left, right, on, ..
            } => {
                left.upgrade_legacy_serialized_dispatches()
                    | right.upgrade_legacy_serialized_dispatches()
                    | upgrade_optional(on)
            }
            Self::Values { rows, .. } => upgrade_rows(rows),
            Self::Function { args, .. } => upgrade_exprs(args),
            Self::FunctionGroup { functions, .. } => {
                functions.iter_mut().fold(false, |changed, function| {
                    upgrade_exprs(&mut function.args) | changed
                })
            }
            Self::Subquery { body, .. } => body.upgrade_legacy_serialized_dispatches(),
        }
    }
}

impl SelectStmt {
    /// Upgrade every legacy compiler dispatch marker in this complete query tree.
    #[doc(hidden)]
    pub fn upgrade_legacy_serialized_dispatches(&mut self) -> bool {
        let mut changed = upgrade_projections(&mut self.projections);
        changed |= upgrade_rows(&mut self.values);
        if let Some(from) = &mut self.from {
            changed |= from.upgrade_legacy_serialized_dispatches();
        }
        changed |= upgrade_optional(&mut self.r#where);
        changed |= upgrade_exprs(&mut self.group_by);
        for grouping_set in &mut self.grouping_sets {
            changed |= upgrade_exprs(grouping_set);
        }
        changed |= upgrade_optional(&mut self.having);
        for order in &mut self.order_by {
            changed |= order.expr.upgrade_legacy_serialized_dispatches();
        }
        changed |= upgrade_optional(&mut self.limit);
        changed |= upgrade_optional(&mut self.offset);
        changed |= upgrade_ctes(&mut self.with);
        if let Some(set_operation) = &mut self.set_op {
            if let Some(left) = &mut set_operation.left {
                changed |= left.upgrade_legacy_serialized_dispatches();
            }
            changed |= set_operation.right.upgrade_legacy_serialized_dispatches();
            for order in &mut set_operation.combined_order_by {
                changed |= order.expr.upgrade_legacy_serialized_dispatches();
            }
            changed |= upgrade_optional(&mut set_operation.combined_limit);
            changed |= upgrade_optional(&mut set_operation.combined_offset);
        }
        changed | upgrade_exprs(&mut self.distinct_on)
    }
}

impl MergeWhen {
    fn upgrade_legacy_serialized_dispatches(&mut self) -> bool {
        match self {
            Self::UpdateMatched {
                condition,
                assignments,
            }
            | Self::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => upgrade_optional(condition) | upgrade_assignments(assignments),
            Self::InsertNotMatched {
                condition, values, ..
            } => upgrade_optional(condition) | upgrade_exprs(values),
            Self::DeleteMatched { condition }
            | Self::DeleteNotMatchedBySource { condition }
            | Self::NothingMatched { condition }
            | Self::NothingNotMatched { condition }
            | Self::NothingNotMatchedBySource { condition } => upgrade_optional(condition),
        }
    }
}

impl Statement {
    /// Upgrade legacy compiler dispatch markers without reparsing SQL or changing catalog-bound relation identities.
    #[doc(hidden)]
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive AST migration preserves every serialized variant"
    )]
    pub fn upgrade_legacy_serialized_dispatches(&mut self) -> bool {
        match self {
            Self::Select(select) => select.upgrade_legacy_serialized_dispatches(),
            Self::Insert(insert) => {
                let mut changed = upgrade_ctes(&mut insert.with);
                changed |= upgrade_rows(&mut insert.rows);
                if let Some(source) = &mut insert.select_source {
                    changed |= source.upgrade_legacy_serialized_dispatches();
                }
                if let Some(conflict) = &mut insert.on_conflict {
                    if let OnConflictAction::Update {
                        assignments,
                        r#where,
                    } = &mut conflict.action
                    {
                        changed |= upgrade_assignments(assignments);
                        changed |= r#where
                            .as_deref_mut()
                            .is_some_and(Expr::upgrade_legacy_serialized_dispatches);
                    }
                }
                changed | upgrade_projections(&mut insert.returning)
            }
            Self::Update(update) => {
                let mut changed = upgrade_assignments(&mut update.assignments);
                changed |= upgrade_optional(&mut update.r#where);
                changed |= upgrade_ctes(&mut update.with);
                if let Some(from) = &mut update.from {
                    changed |= from.upgrade_legacy_serialized_dispatches();
                }
                changed | upgrade_projections(&mut update.returning)
            }
            Self::Delete(delete) => {
                let mut changed = upgrade_optional(&mut delete.r#where);
                changed |= upgrade_ctes(&mut delete.with);
                if let Some(using) = &mut delete.using {
                    changed |= using.upgrade_legacy_serialized_dispatches();
                }
                changed | upgrade_projections(&mut delete.returning)
            }
            Self::CreateView { body, .. }
            | Self::CreateMaterializedView { body, .. }
            | Self::CreateTableAs { body, .. } => body.upgrade_legacy_serialized_dispatches(),
            Self::DeclareCursor(cursor) => cursor.query.upgrade_legacy_serialized_dispatches(),
            Self::Explain { body, .. } | Self::Prepare { body, .. } => {
                body.upgrade_legacy_serialized_dispatches()
            }
            Self::Execute { params, .. } | Self::Call { args: params, .. } => upgrade_exprs(params),
            Self::Values { rows } => upgrade_rows(rows),
            Self::Merge(merge) => {
                let mut changed = merge.source.upgrade_legacy_serialized_dispatches();
                changed |= merge.join_condition.upgrade_legacy_serialized_dispatches();
                for clause in &mut merge.when_clauses {
                    changed |= clause.upgrade_legacy_serialized_dispatches();
                }
                changed | upgrade_projections(&mut merge.returning)
            }
            Self::CreateFunction(definition) => {
                let mut changed = definition
                    .params
                    .iter_mut()
                    .fold(false, |changed, parameter| {
                        parameter
                            .default
                            .as_mut()
                            .is_some_and(Expr::upgrade_legacy_serialized_dispatches)
                            | changed
                    });
                if let FunctionBody::Statements(statements) = &mut definition.body {
                    for statement in statements {
                        changed |= statement.upgrade_legacy_serialized_dispatches();
                    }
                }
                changed
            }
            Self::CreateTrigger(trigger) => upgrade_optional(&mut trigger.when),
            Self::CreateRule(rule) => {
                let mut changed = upgrade_optional(&mut rule.condition);
                for action in &mut rule.actions {
                    changed |= action.upgrade_legacy_serialized_dispatches();
                }
                changed
            }
            Self::CreateTable(_)
            | Self::CreateIndex(_)
            | Self::Drop(_)
            | Self::AlterTable(_)
            | Self::AlterForeignTable(_)
            | Self::AlterView(_)
            | Self::RefreshMaterializedView { .. }
            | Self::CreateSchema { .. }
            | Self::Notify { .. }
            | Self::Listen { .. }
            | Self::Unlisten { .. }
            | Self::SetVariable { .. }
            | Self::ResetVariable { .. }
            | Self::ResetAllVariables
            | Self::SetConstraints { .. }
            | Self::ShowVariable { .. }
            | Self::Discard { .. }
            | Self::Load { .. }
            | Self::Analyze { .. }
            | Self::Vacuum(_)
            | Self::Truncate { .. }
            | Self::Transaction(_)
            | Self::FetchCursor(_)
            | Self::CloseCursor { .. }
            | Self::CreateSequence(_)
            | Self::AlterSequence(_)
            | Self::Deallocate { .. }
            | Self::CreateForeignServer(_)
            | Self::CreateForeignTable(_)
            | Self::DropFunction(_)
            | Self::AlterRoutine(_)
            | Self::AlterRoutineOwner(_)
            | Self::RenameRoutine(_)
            | Self::GrantRoutine(_)
            | Self::GrantTable(_)
            | Self::GrantSequence(_)
            | Self::GrantDatabase(_)
            | Self::GrantSchema(_)
            | Self::GrantRole(_)
            | Self::CreateRole(_)
            | Self::AlterRole(_)
            | Self::DropRole(_)
            | Self::DropTrigger(_)
            | Self::DropRule(_)
            | Self::DoBlock { .. } => false,
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
