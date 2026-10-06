//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound copies of stored syntax, read back in the order of that syntax. Catalog-owned expressions and statements are stored as syntax and bound by lowering a copy; binding keeps the copy's shape, so reading the lowered copy and its bound form side by side, in the order the stored syntax visitor walks the syntax, pairs every routine call, cast and `unknown` literal with the syntax node it came from. The lowered copy tells which bound nodes stand for syntax literals: variable binding may replace column references by typed placeholders, which are not syntax.

use super::stored_routines::BoundRoutineReference;
use crate::plan::CommandPlan;
use crate::plan::{CtePlanBody, ExpressionPlan, QueryPlan, RelationalPlan, SourcePlan};
use crate::{SQLError, ScalarExpr, ScalarFrameBound};
use uqa_core::Value;

/// An expression site of stored syntax and what binding recorded for it.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueSite {
    /// A cast, with the type name binding gave it.
    Cast(String),
    /// An `unknown` literal that binding left unconverted.
    Literal,
    /// An `unknown` literal that binding converted to a typed constant.
    Constant { value: Value, ty: String },
    /// A cast binding wrapped around the syntax, coercing an operand to the type its operator declares, as a `RelabelType` or an implicit coercion does.
    Relabel(String),
    /// A function's written ordering syntax, including a legacy call whose selected binding recovered the distinction.
    FunctionOrder(crate::ast::FunctionOrderSyntax),
    /// Any other node of a stored expression, which keeps the sites in step with the syntax so that a relabel reaches the node it wraps.
    Node,
}

/// The casts a node begins with.
fn cast_depth(expression: &ScalarExpr) -> usize {
    let mut depth = 0;
    let mut node = expression;
    while let ScalarExpr::Cast { expr, .. } = node {
        depth += 1;
        node = expr;
    }
    depth
}

/// Everything binding recorded for one piece of stored syntax, in syntax order.
#[derive(Debug, Default)]
pub struct SyntaxSites {
    /// Routine calls, each reported after its arguments as the syntax visitor reports calls.
    pub routines: Vec<BoundRoutineReference>,
    /// Casts, `unknown` literals and function ordering syntax in pre-order.
    pub values: Vec<ValueSite>,
}

/// Sites of a stored expression: `lowered` is the plan lowered from the syntax and `bound` its bound copy. The expression visitor reads a site at every node, so the casts binding adds around a node reach it as relabels.
pub fn expression_syntax_sites(
    lowered: &ExpressionPlan,
    bound: &ExpressionPlan,
) -> Result<SyntaxSites, SQLError> {
    let mut walk = Walk {
        aligned: true,
        ..Walk::default()
    };
    walk.scalar(
        &lowered.scalar,
        &bound.scalar,
        (&lowered.subqueries, &bound.subqueries),
    )?;
    Ok(walk.sites)
}

/// Sites of a stored statement lowered to a query.
pub fn query_syntax_sites(lowered: &QueryPlan, bound: &QueryPlan) -> Result<SyntaxSites, SQLError> {
    let mut walk = Walk::default();
    walk.query(lowered, bound)?;
    Ok(walk.sites)
}

type Subqueries<'a> = (&'a [QueryPlan], &'a [QueryPlan]);

fn shape_error(what: &str) -> SQLError {
    SQLError::Internal(format!("bound stored syntax no longer matches its {what}"))
}

fn pairs<'a, T>(
    lowered: &'a [T],
    bound: &'a [T],
    what: &str,
) -> Result<impl Iterator<Item = (&'a T, &'a T)>, SQLError> {
    if lowered.len() != bound.len() {
        return Err(shape_error(what));
    }
    Ok(lowered.iter().zip(bound))
}

fn optional_pair<'a, T>(
    lowered: Option<&'a T>,
    bound: Option<&'a T>,
    what: &str,
) -> Result<Option<(&'a T, &'a T)>, SQLError> {
    match (lowered, bound) {
        (Some(lowered), Some(bound)) => Ok(Some((lowered, bound))),
        (None, None) => Ok(None),
        _ => Err(shape_error(what)),
    }
}

#[derive(Default)]
struct Walk {
    sites: SyntaxSites,
    /// Whether every node records a site. The expression visitor keeps in step with the walk node by node, so a relabel can name the node it wraps; the statement visitor pairs casts and literals only, so the casts binding adds are left to binding at execution and recorded for no node.
    aligned: bool,
}

impl Walk {
    fn query(&mut self, lowered: &QueryPlan, bound: &QueryPlan) -> Result<(), SQLError> {
        for (lowered, bound) in pairs(&lowered.ctes, &bound.ctes, "common table expressions")? {
            self.cte_body(&lowered.body, &bound.body)?;
            if let Some((lowered, bound)) =
                optional_pair(lowered.cycle.as_ref(), bound.cycle.as_ref(), "CYCLE clause")?
            {
                self.scalar(&lowered.mark_value, &bound.mark_value, (&[], &[]))?;
                self.scalar(&lowered.mark_default, &bound.mark_default, (&[], &[]))?;
            }
        }
        match (&lowered.root, &bound.root) {
            (RelationalPlan::QueryBlock(lowered), RelationalPlan::QueryBlock(bound)) => {
                let subqueries = (&lowered.subqueries[..], &bound.subqueries[..]);
                if let Some((lowered, bound)) =
                    optional_pair(lowered.from.as_ref(), bound.from.as_ref(), "FROM clause")?
                {
                    self.source(lowered, bound, subqueries)?;
                }
                for (lowered, bound) in
                    pairs(&lowered.projections, &bound.projections, "select list")?
                {
                    self.scalar(&lowered.expr, &bound.expr, subqueries)?;
                }
                self.optional(lowered.r#where.as_ref(), bound.r#where.as_ref(), subqueries)?;
                self.scalars(&lowered.group_by, &bound.group_by, subqueries)?;
                if lowered.grouping_sets.len() != bound.grouping_sets.len() {
                    return Err(shape_error("grouping sets"));
                }
                for (lowered, bound) in lowered.grouping_sets.iter().zip(&bound.grouping_sets) {
                    self.scalars(lowered, bound, subqueries)?;
                }
                self.optional(lowered.having.as_ref(), bound.having.as_ref(), subqueries)?;
                for (lowered, bound) in pairs(&lowered.order_by, &bound.order_by, "ORDER BY")? {
                    self.scalar(&lowered.expr, &bound.expr, subqueries)?;
                }
                self.optional(lowered.limit.as_ref(), bound.limit.as_ref(), subqueries)?;
                self.optional(lowered.offset.as_ref(), bound.offset.as_ref(), subqueries)?;
                self.scalars(&lowered.distinct_on, &bound.distinct_on, subqueries)?;
                for (lowered, bound) in
                    pairs(&lowered.windows, &bound.windows, "WINDOW definitions")?
                {
                    if lowered.name != bound.name || lowered.inherited != bound.inherited {
                        return Err(shape_error("WINDOW identity"));
                    }
                    let lowered: Vec<_> = lowered.spec.expressions().collect();
                    let bound: Vec<_> = bound.spec.expressions().collect();
                    for (lowered, bound) in pairs(&lowered, &bound, "WINDOW expressions")? {
                        self.scalar(lowered, bound, subqueries)?;
                    }
                }
                Ok(())
            }
            (
                RelationalPlan::SetOp {
                    left,
                    right,
                    order_by,
                    limit,
                    offset,
                    subqueries,
                    ..
                },
                RelationalPlan::SetOp {
                    left: bound_left,
                    right: bound_right,
                    order_by: bound_order,
                    limit: bound_limit,
                    offset: bound_offset,
                    subqueries: bound_subqueries,
                    ..
                },
            ) => {
                self.query(left, bound_left)?;
                self.query(right, bound_right)?;
                let subqueries = (&subqueries[..], &bound_subqueries[..]);
                for (lowered, bound) in pairs(order_by, bound_order, "ORDER BY")? {
                    self.scalar(&lowered.expr, &bound.expr, subqueries)?;
                }
                self.optional(limit.as_deref(), bound_limit.as_deref(), subqueries)?;
                self.optional(offset.as_deref(), bound_offset.as_deref(), subqueries)
            }
            (
                RelationalPlan::Values { rows, subqueries },
                RelationalPlan::Values {
                    rows: bound_rows,
                    subqueries: bound_subqueries,
                },
            ) => {
                let subqueries = (&subqueries[..], &bound_subqueries[..]);
                for (lowered, bound) in pairs(rows, bound_rows, "VALUES list")? {
                    self.scalars(lowered, bound, subqueries)?;
                }
                Ok(())
            }
            _ => Err(shape_error("query")),
        }
    }

    /// A data-modifying `WITH` query, in the order the syntax visitor walks the statement.
    fn cte_body(&mut self, lowered: &CtePlanBody, bound: &CtePlanBody) -> Result<(), SQLError> {
        match (lowered, bound) {
            (CtePlanBody::Query(lowered), CtePlanBody::Query(bound)) => self.query(lowered, bound),
            (CtePlanBody::Command(lowered), CtePlanBody::Command(bound)) => {
                self.command(lowered, bound)
            }
            _ => Err(shape_error("WITH query")),
        }
    }

    fn command(&mut self, lowered: &CommandPlan, bound: &CommandPlan) -> Result<(), SQLError> {
        for (lowered, bound) in pairs(lowered.ctes(), bound.ctes(), "common table expressions")? {
            self.cte_body(&lowered.body, &bound.body)?;
            if let Some((lowered, bound)) =
                optional_pair(lowered.cycle.as_ref(), bound.cycle.as_ref(), "CYCLE clause")?
            {
                self.scalar(&lowered.mark_value, &bound.mark_value, (&[], &[]))?;
                self.scalar(&lowered.mark_default, &bound.mark_default, (&[], &[]))?;
            }
        }
        let subqueries = (lowered.scalar_subqueries(), bound.scalar_subqueries());
        match (lowered, bound) {
            (CommandPlan::Insert(lowered), CommandPlan::Insert(bound)) => {
                if let Some((lowered, bound)) = optional_pair(
                    lowered.source.as_deref(),
                    bound.source.as_deref(),
                    "INSERT source",
                )? {
                    self.query(lowered, bound)?;
                }
            }
            (CommandPlan::Update(_), CommandPlan::Update(_))
            | (CommandPlan::Delete(_), CommandPlan::Delete(_))
            | (CommandPlan::Merge(_), CommandPlan::Merge(_)) => {
                if let Some((lowered, bound)) = optional_pair(
                    lowered.source_input(),
                    bound.source_input(),
                    "command source",
                )? {
                    self.source(lowered, bound, subqueries)?;
                }
            }
            _ => return Err(shape_error("WITH command")),
        }
        let lowered = syntax_expressions(lowered);
        let bound = syntax_expressions(bound);
        for (lowered, bound) in pairs(&lowered, &bound, "command expressions")? {
            self.scalar(lowered, bound, subqueries)?;
        }
        Ok(())
    }

    fn source(
        &mut self,
        lowered: &SourcePlan,
        bound: &SourcePlan,
        subqueries: Subqueries<'_>,
    ) -> Result<(), SQLError> {
        match (lowered, bound) {
            (SourcePlan::Table { .. }, SourcePlan::Table { .. }) => Ok(()),
            (
                SourcePlan::Join {
                    left, right, on, ..
                },
                SourcePlan::Join {
                    left: bound_left,
                    right: bound_right,
                    on: bound_on,
                    ..
                },
            ) => {
                self.source(left, bound_left, subqueries)?;
                self.source(right, bound_right, subqueries)?;
                self.optional(on.as_ref(), bound_on.as_ref(), subqueries)
            }
            (
                SourcePlan::Values { rows, .. },
                SourcePlan::Values {
                    rows: bound_rows, ..
                },
            ) => {
                for (lowered, bound) in pairs(rows, bound_rows, "VALUES list")? {
                    self.scalars(lowered, bound, subqueries)?;
                }
                Ok(())
            }
            (
                SourcePlan::Function { args, .. },
                SourcePlan::Function {
                    name,
                    binding,
                    args: bound_args,
                    ..
                },
            ) => {
                self.sites.routines.push(BoundRoutineReference {
                    name: name.clone(),
                    binding: binding.clone(),
                });
                self.scalars(args, bound_args, subqueries)
            }
            (
                SourcePlan::FunctionGroup { functions, .. },
                SourcePlan::FunctionGroup {
                    functions: bound_functions,
                    ..
                },
            ) => {
                for (lowered, bound) in pairs(functions, bound_functions, "ROWS FROM list")? {
                    self.sites.routines.push(BoundRoutineReference {
                        name: bound.name.clone(),
                        binding: bound.binding.clone(),
                    });
                    self.scalars(&lowered.args, &bound.args, subqueries)?;
                }
                Ok(())
            }
            (SourcePlan::Subquery { body, .. }, SourcePlan::Subquery { body: bound, .. }) => {
                self.query(body, bound)
            }
            _ => Err(shape_error("FROM item")),
        }
    }

    fn optional(
        &mut self,
        lowered: Option<&ScalarExpr>,
        bound: Option<&ScalarExpr>,
        subqueries: Subqueries<'_>,
    ) -> Result<(), SQLError> {
        if let Some((lowered, bound)) = optional_pair(lowered, bound, "clause")? {
            self.scalar(lowered, bound, subqueries)?;
        }
        Ok(())
    }

    fn scalars(
        &mut self,
        lowered: &[ScalarExpr],
        bound: &[ScalarExpr],
        subqueries: Subqueries<'_>,
    ) -> Result<(), SQLError> {
        for (lowered, bound) in pairs(lowered, bound, "expression list")? {
            self.scalar(lowered, bound, subqueries)?;
        }
        Ok(())
    }

    fn subquery(&mut self, index: usize, subqueries: Subqueries<'_>) -> Result<(), SQLError> {
        match (subqueries.0.get(index), subqueries.1.get(index)) {
            (Some(lowered), Some(bound)) => self.query(lowered, bound),
            _ => Err(SQLError::Internal(format!(
                "stored syntax cannot resolve subquery slot {index}"
            ))),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one syntax-order walk covers every scalar variant"
    )]
    fn scalar(
        &mut self,
        lowered: &ScalarExpr,
        bound: &ScalarExpr,
        subqueries: Subqueries<'_>,
    ) -> Result<(), SQLError> {
        // Binding wraps an operand in the casts its operator needs, outside the casts the syntax writes.
        let mut bound = bound;
        for _ in 0..cast_depth(bound).saturating_sub(cast_depth(lowered)) {
            let ScalarExpr::Cast { expr, ty } = bound else {
                return Err(shape_error("relabel"));
            };
            if self.aligned {
                self.sites.values.push(ValueSite::Relabel(ty.clone()));
            }
            bound = expr;
        }
        match lowered {
            ScalarExpr::Literal(Value::Str(_) | Value::Null) => {
                self.sites.values.push(match bound {
                    ScalarExpr::TypedLiteral { value, ty, .. } => ValueSite::Constant {
                        value: value.clone(),
                        ty: ty.clone(),
                    },
                    ScalarExpr::Literal(Value::Str(_) | Value::Null) => ValueSite::Literal,
                    _ => return Err(shape_error("literal")),
                });
                return Ok(());
            }
            ScalarExpr::Cast { .. } => {
                let ScalarExpr::Cast { ty, .. } = bound else {
                    return Err(shape_error("cast"));
                };
                self.sites.values.push(ValueSite::Cast(ty.clone()));
            }
            ScalarExpr::Func { order_syntax, .. } => {
                let ScalarExpr::Func {
                    order_syntax: bound,
                    ..
                } = bound
                else {
                    return Err(shape_error("function ordering"));
                };
                if !order_syntax.is_legacy() && order_syntax != bound {
                    return Err(shape_error("function ordering"));
                }
                self.sites.values.push(ValueSite::FunctionOrder(*bound));
            }
            _ => {
                if self.aligned {
                    self.sites.values.push(ValueSite::Node);
                }
            }
        }
        match (lowered, bound) {
            (
                ScalarExpr::Func {
                    args,
                    order_by,
                    filter,
                    ..
                },
                ScalarExpr::Func {
                    name,
                    binding,
                    args: bound_args,
                    order_by: bound_order,
                    filter: bound_filter,
                    ..
                },
            ) => {
                self.scalars(args, bound_args, subqueries)?;
                for (lowered, bound) in pairs(order_by, bound_order, "aggregate ORDER BY")? {
                    self.scalar(&lowered.expr, &bound.expr, subqueries)?;
                }
                self.optional(filter.as_deref(), bound_filter.as_deref(), subqueries)?;
                self.sites.routines.push(BoundRoutineReference {
                    name: name.clone(),
                    binding: binding.clone(),
                });
                Ok(())
            }
            (ScalarExpr::Array(items), ScalarExpr::Array(bound))
            | (ScalarExpr::Row(items), ScalarExpr::Row(bound))
            | (ScalarExpr::And(items), ScalarExpr::And(bound))
            | (ScalarExpr::Or(items), ScalarExpr::Or(bound)) => {
                self.scalars(items, bound, subqueries)
            }
            (
                ScalarExpr::Binary { lhs, rhs, .. },
                ScalarExpr::Binary {
                    lhs: bound_lhs,
                    rhs: bound_rhs,
                    ..
                },
            ) => {
                self.scalar(lhs, bound_lhs, subqueries)?;
                self.scalar(rhs, bound_rhs, subqueries)
            }
            (ScalarExpr::UnaryMinus(inner), ScalarExpr::UnaryMinus(bound))
            | (ScalarExpr::Not(inner), ScalarExpr::Not(bound))
            | (ScalarExpr::IsNull { expr: inner, .. }, ScalarExpr::IsNull { expr: bound, .. })
            | (ScalarExpr::Cast { expr: inner, .. }, ScalarExpr::Cast { expr: bound, .. }) => {
                self.scalar(inner, bound, subqueries)
            }
            (
                ScalarExpr::Between { expr, low, high },
                ScalarExpr::Between {
                    expr: bound_expr,
                    low: bound_low,
                    high: bound_high,
                },
            ) => {
                self.scalar(expr, bound_expr, subqueries)?;
                self.scalar(low, bound_low, subqueries)?;
                self.scalar(high, bound_high, subqueries)
            }
            (
                ScalarExpr::InList { expr, list, .. },
                ScalarExpr::InList {
                    expr: bound_expr,
                    list: bound_list,
                    ..
                },
            ) => {
                self.scalar(expr, bound_expr, subqueries)?;
                self.scalars(list, bound_list, subqueries)
            }
            (
                ScalarExpr::WindowCall {
                    args, spec, filter, ..
                },
                ScalarExpr::WindowCall {
                    name,
                    args: bound_args,
                    spec: bound_spec,
                    filter: bound_filter,
                    ..
                },
            ) => {
                self.scalars(args, bound_args, subqueries)?;
                if let Some((lowered, bound)) =
                    optional_pair(filter.as_deref(), bound_filter.as_deref(), "window FILTER")?
                {
                    self.scalar(lowered, bound, subqueries)?;
                }
                self.scalars(&spec.partition_by, &bound_spec.partition_by, subqueries)?;
                for (lowered, bound) in
                    pairs(&spec.order_by, &bound_spec.order_by, "window ORDER BY")?
                {
                    self.scalar(&lowered.expr, &bound.expr, subqueries)?;
                }
                if let Some((lowered, bound)) = optional_pair(
                    spec.frame.as_ref(),
                    bound_spec.frame.as_ref(),
                    "window frame",
                )? {
                    self.frame_bound(&lowered.start, &bound.start, subqueries)?;
                    self.frame_bound(&lowered.end, &bound.end, subqueries)?;
                }
                self.sites.routines.push(BoundRoutineReference {
                    name: name.clone(),
                    binding: None,
                });
                Ok(())
            }
            (
                ScalarExpr::Case {
                    base,
                    when,
                    else_branch,
                },
                ScalarExpr::Case {
                    base: bound_base,
                    when: bound_when,
                    else_branch: bound_else,
                },
            ) => {
                self.optional(base.as_deref(), bound_base.as_deref(), subqueries)?;
                for ((condition, result), (bound_condition, bound_result)) in
                    pairs(when, bound_when, "CASE arms")?
                {
                    self.scalar(condition, bound_condition, subqueries)?;
                    self.scalar(result, bound_result, subqueries)?;
                }
                self.optional(else_branch.as_deref(), bound_else.as_deref(), subqueries)
            }
            (ScalarExpr::ScalarSubquery(index), ScalarExpr::ScalarSubquery(bound))
            | (
                ScalarExpr::Exists {
                    subquery: index, ..
                },
                ScalarExpr::Exists {
                    subquery: bound, ..
                },
            ) if index == bound => self.subquery(*index, subqueries),
            (
                ScalarExpr::InSubquery {
                    expr,
                    subquery: index,
                    ..
                },
                ScalarExpr::InSubquery {
                    expr: bound_expr,
                    subquery: bound,
                    ..
                },
            ) if index == bound => {
                self.scalar(expr, bound_expr, subqueries)?;
                self.subquery(*index, subqueries)
            }
            // Leaves: binding may replace a column by its structural reference or a typed placeholder.
            (
                ScalarExpr::Star
                | ScalarExpr::QualifiedStar(_)
                | ScalarExpr::Default
                | ScalarExpr::Column(_)
                | ScalarExpr::Position(_)
                | ScalarExpr::InternalColumn(_)
                | ScalarExpr::QualifiedColumn { .. }
                | ScalarExpr::Literal(_)
                | ScalarExpr::TypedLiteral { .. }
                | ScalarExpr::Param(_),
                _,
            ) => Ok(()),
            _ => Err(shape_error("expression")),
        }
    }

    fn frame_bound(
        &mut self,
        lowered: &ScalarFrameBound,
        bound: &ScalarFrameBound,
        subqueries: Subqueries<'_>,
    ) -> Result<(), SQLError> {
        match (lowered, bound) {
            (ScalarFrameBound::Preceding(lowered), ScalarFrameBound::Preceding(bound))
            | (ScalarFrameBound::Following(lowered), ScalarFrameBound::Following(bound)) => {
                self.scalar(lowered, bound, subqueries)
            }
            (
                ScalarFrameBound::UnboundedPreceding
                | ScalarFrameBound::UnboundedFollowing
                | ScalarFrameBound::CurrentRow,
                _,
            ) => Ok(()),
            _ => Err(shape_error("window frame")),
        }
    }
}

/// The scalar expressions a data-modifying `WITH` query owns in syntax order; planner-added view checks and target predicates have no syntax.
fn syntax_expressions(command: &CommandPlan) -> Vec<&ScalarExpr> {
    use crate::plan::{ConflictActionPlan, MergeWhenPlan};
    let mut expressions = Vec::new();
    match command {
        CommandPlan::Insert(plan) => {
            expressions.extend(
                plan.columns
                    .iter()
                    .flat_map(crate::ast::AssignmentTarget::expressions),
            );
            expressions.extend(plan.rows.iter().flatten());
            if let Some(conflict) = &plan.on_conflict {
                expressions.extend(&conflict.expressions);
                expressions.extend(conflict.predicate.as_deref());
                if let ConflictActionPlan::Update {
                    assignments,
                    predicate,
                } = &conflict.action
                {
                    expressions.extend(
                        assignments
                            .iter()
                            .flat_map(crate::plan::AssignmentPlan::expressions),
                    );
                    expressions.extend(predicate.as_deref());
                }
            }
            expressions.extend(plan.returning.iter().map(|projection| &projection.expr));
        }
        CommandPlan::Update(plan) => {
            expressions.extend(
                plan.assignments
                    .iter()
                    .flat_map(crate::plan::AssignmentPlan::expressions),
            );
            expressions.extend(plan.predicate.as_ref());
            expressions.extend(plan.returning.iter().map(|projection| &projection.expr));
        }
        CommandPlan::Delete(plan) => {
            expressions.extend(plan.predicate.as_ref());
            expressions.extend(plan.returning.iter().map(|projection| &projection.expr));
        }
        CommandPlan::Merge(plan) => {
            expressions.push(&plan.join_condition);
            for clause in &plan.when_clauses {
                match clause {
                    MergeWhenPlan::UpdateMatched {
                        condition,
                        assignments,
                    }
                    | MergeWhenPlan::UpdateNotMatchedBySource {
                        condition,
                        assignments,
                    } => {
                        expressions.extend(condition.as_ref());
                        expressions.extend(
                            assignments
                                .iter()
                                .flat_map(crate::plan::AssignmentPlan::expressions),
                        );
                    }
                    MergeWhenPlan::InsertNotMatched {
                        condition,
                        columns,
                        values,
                        ..
                    } => {
                        expressions.extend(condition.as_ref());
                        expressions.extend(
                            columns
                                .iter()
                                .flat_map(crate::ast::AssignmentTarget::expressions),
                        );
                        expressions.extend(values.iter());
                    }
                    MergeWhenPlan::DeleteMatched { condition }
                    | MergeWhenPlan::DeleteNotMatchedBySource { condition }
                    | MergeWhenPlan::NothingMatched { condition }
                    | MergeWhenPlan::NothingNotMatched { condition }
                    | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                        expressions.extend(condition.as_ref());
                    }
                }
            }
            expressions.extend(plan.returning.iter().map(|projection| &projection.expr));
        }
        _ => {}
    }
    expressions
}
