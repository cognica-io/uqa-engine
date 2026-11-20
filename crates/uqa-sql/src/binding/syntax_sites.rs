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
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

mod commands;
mod input;
pub(crate) use input::SiteInput;
use input::Walk;

/// An expression site of stored syntax and what binding recorded for it.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueSite {
    /// A collation clause removed by the selected operand coercion.
    DiscardCollation,
    /// A ROW DISTINCT predicate expanded into its selected field comparisons.
    RowDistinct,
    /// Positions grouped into a scalar-array comparison; the other IN items become individual comparisons.
    Membership { array_items: Vec<usize> },
    /// A cast, with the type name binding gave it.
    Cast(String),
    /// A ROW constructor bound to its original composite attribute positions.
    CompositeRow(crate::ast::CompositeRowBinding),
    /// An `unknown` literal that binding left unconverted.
    Literal,
    /// An `unknown` literal that binding converted to a typed constant.
    Constant { value: Value, ty: String },
    /// A cast binding wrapped around the syntax, coercing an operand to the type its operator declares, as a `RelabelType` or an implicit coercion does.
    Relabel(String),
    /// A function's written ordering syntax, including a legacy call whose selected binding recovered the distinction.
    FunctionOrder(crate::ast::FunctionOrderSyntax),
    /// Any other node of stored syntax, which keeps the sites in step with the syntax so that a relabel reaches the node it wraps.
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
    let control = ProductionControl::uncontrolled();
    Ok(
        crate::catalog::work::inline(expression_sites_from(lowered, bound, &control, &mut ()))?
            .into_uncontrolled()
            .expect("ordinary syntax sites"),
    )
}

/// Capture the correspondence with its original source pair and admitted position buffers until all stored edits finish.
pub async fn expression_syntax_sites_with_work(
    lowered: &ExpressionPlan,
    bound: &ExpressionPlan,
    control: &ProductionControl<'_>,
    work: &mut AsyncImageInput<'_>,
) -> Result<Produced<SyntaxSites>, SQLError> {
    expression_sites_from(lowered, bound, control, work).await
}

pub(crate) async fn expression_sites_from<W: SiteInput>(
    lowered: &ExpressionPlan,
    bound: &ExpressionPlan,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<SyntaxSites>, SQLError>
where
    W::Error: Into<SQLError>,
{
    let mut walk = Walk::new(*control, work);
    walk.scalar(
        &lowered.scalar,
        &bound.scalar,
        (&lowered.subqueries, &bound.subqueries),
    )
    .await?;
    walk.finish()
}

/// Sites of a stored statement lowered to a query.
pub fn query_syntax_sites(lowered: &QueryPlan, bound: &QueryPlan) -> Result<SyntaxSites, SQLError> {
    ordinary_query_sites(lowered, bound, false)
}

/// Sites from the query adapter for a stored command; its final output is synthetic and has no syntax node.
pub(super) fn command_query_syntax_sites(
    lowered: &QueryPlan,
    bound: &QueryPlan,
) -> Result<SyntaxSites, SQLError> {
    ordinary_query_sites(lowered, bound, true)
}

fn ordinary_query_sites(
    lowered: &QueryPlan,
    bound: &QueryPlan,
    command: bool,
) -> Result<SyntaxSites, SQLError> {
    let control = ProductionControl::uncontrolled();
    Ok(crate::catalog::work::inline(query_syntax_sites_with_work(
        lowered,
        bound,
        command,
        &control,
        &mut (),
    ))?
    .into_uncontrolled()
    .expect("ordinary syntax sites"))
}

pub(super) async fn query_syntax_sites_with_work<W: SiteInput>(
    lowered: &QueryPlan,
    bound: &QueryPlan,
    command: bool,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<SyntaxSites>, SQLError>
where
    W::Error: Into<SQLError>,
{
    let mut walk = Walk::new(*control, work);
    walk.query(lowered, bound, command).await?;
    walk.finish()
}

fn syntax_projections(
    projections: &[crate::plan::ProjectionPlan],
    command_root: bool,
) -> Result<&[crate::plan::ProjectionPlan], SQLError> {
    if command_root {
        projections
            .split_last()
            .map(|(_, syntax)| syntax)
            .ok_or_else(|| shape_error("command output"))
    } else {
        Ok(projections)
    }
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

impl<W: SiteInput> Walk<'_, '_, W>
where
    W::Error: Into<SQLError>,
{
    async fn query(
        &mut self,
        lowered: &QueryPlan,
        bound: &QueryPlan,
        command_root: bool,
    ) -> Result<(), SQLError> {
        self.checkpoint().await?;
        for (lowered, bound) in pairs(&lowered.ctes, &bound.ctes, "common table expressions")? {
            self.cte_body(&lowered.body, &bound.body).await?;
            if let Some((lowered, bound)) =
                optional_pair(lowered.cycle.as_ref(), bound.cycle.as_ref(), "CYCLE clause")?
            {
                self.scalar(&lowered.mark_value, &bound.mark_value, (&[], &[]))
                    .await?;
                self.scalar(&lowered.mark_default, &bound.mark_default, (&[], &[]))
                    .await?;
            }
        }
        match (&lowered.root, &bound.root) {
            (RelationalPlan::QueryBlock(lowered), RelationalPlan::QueryBlock(bound)) => {
                self.query_block(lowered, bound, command_root).await
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
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.query(left, bound_left, false)
                )?;
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.query(right, bound_right, false)
                )?;
                let subqueries = (&subqueries[..], &bound_subqueries[..]);
                for (lowered, bound) in pairs(order_by, bound_order, "ORDER BY")? {
                    self.scalar(&lowered.expr, &bound.expr, subqueries).await?;
                }
                self.optional(limit.as_deref(), bound_limit.as_deref(), subqueries)
                    .await?;
                self.optional(offset.as_deref(), bound_offset.as_deref(), subqueries)
                    .await
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
                    self.scalars(lowered, bound, subqueries).await?;
                }
                Ok(())
            }
            _ => Err(shape_error("query")),
        }
    }

    async fn query_block(
        &mut self,
        lowered: &crate::plan::QueryBlockPlan,
        bound: &crate::plan::QueryBlockPlan,
        command_root: bool,
    ) -> Result<(), SQLError> {
        let subqueries = (&lowered.subqueries[..], &bound.subqueries[..]);
        if let Some((lowered, bound)) =
            optional_pair(lowered.from.as_ref(), bound.from.as_ref(), "FROM clause")?
        {
            self.source(lowered, bound, subqueries).await?;
        }
        for (lowered, bound) in pairs(
            syntax_projections(&lowered.projections, command_root)?,
            syntax_projections(&bound.projections, command_root)?,
            "select list",
        )? {
            self.scalar(&lowered.expr, &bound.expr, subqueries).await?;
        }
        self.optional(lowered.r#where.as_ref(), bound.r#where.as_ref(), subqueries)
            .await?;
        self.scalars(&lowered.group_by, &bound.group_by, subqueries)
            .await?;
        if lowered.grouping_sets.len() != bound.grouping_sets.len() {
            return Err(shape_error("grouping sets"));
        }
        for (lowered, bound) in lowered.grouping_sets.iter().zip(&bound.grouping_sets) {
            self.scalars(lowered, bound, subqueries).await?;
        }
        self.optional(lowered.having.as_ref(), bound.having.as_ref(), subqueries)
            .await?;
        for (lowered, bound) in pairs(&lowered.order_by, &bound.order_by, "ORDER BY")? {
            self.scalar(&lowered.expr, &bound.expr, subqueries).await?;
        }
        self.optional(lowered.limit.as_ref(), bound.limit.as_ref(), subqueries)
            .await?;
        self.optional(lowered.offset.as_ref(), bound.offset.as_ref(), subqueries)
            .await?;
        self.scalars(&lowered.distinct_on, &bound.distinct_on, subqueries)
            .await?;
        for (lowered, bound) in pairs(&lowered.windows, &bound.windows, "WINDOW definitions")? {
            if lowered.name != bound.name || lowered.inherited != bound.inherited {
                return Err(shape_error("WINDOW identity"));
            }
            let lowered: Vec<_> = lowered.spec.expressions().collect();
            let bound: Vec<_> = bound.spec.expressions().collect();
            for (lowered, bound) in pairs(&lowered, &bound, "WINDOW expressions")? {
                self.scalar(lowered, bound, subqueries).await?;
            }
        }
        Ok(())
    }

    /// A data-modifying `WITH` query, in the order the syntax visitor walks the statement.
    async fn cte_body(
        &mut self,
        lowered: &CtePlanBody,
        bound: &CtePlanBody,
    ) -> Result<(), SQLError> {
        match (lowered, bound) {
            (CtePlanBody::Query(lowered), CtePlanBody::Query(bound)) => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.query(lowered, bound, false)
                )
            }
            (CtePlanBody::Command(lowered), CtePlanBody::Command(bound)) => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.command(lowered, bound)
                )
            }
            _ => Err(shape_error("WITH query")),
        }
    }

    async fn command(
        &mut self,
        lowered: &CommandPlan,
        bound: &CommandPlan,
    ) -> Result<(), SQLError> {
        self.checkpoint().await?;
        for (lowered, bound) in pairs(lowered.ctes(), bound.ctes(), "common table expressions")? {
            self.cte_body(&lowered.body, &bound.body).await?;
            if let Some((lowered, bound)) =
                optional_pair(lowered.cycle.as_ref(), bound.cycle.as_ref(), "CYCLE clause")?
            {
                self.scalar(&lowered.mark_value, &bound.mark_value, (&[], &[]))
                    .await?;
                self.scalar(&lowered.mark_default, &bound.mark_default, (&[], &[]))
                    .await?;
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
                    crate::catalog::work::nested!(
                        W::MAY_SUSPEND,
                        self.control,
                        self.query(lowered, bound, false)
                    )?;
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
                    self.source(lowered, bound, subqueries).await?;
                }
            }
            _ => return Err(shape_error("WITH command")),
        }
        self.command_scalar_fields(lowered, bound, subqueries).await
    }

    async fn source(
        &mut self,
        lowered: &SourcePlan,
        bound: &SourcePlan,
        subqueries: Subqueries<'_>,
    ) -> Result<(), SQLError> {
        self.checkpoint().await?;
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
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.source(left, bound_left, subqueries)
                )?;
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.source(right, bound_right, subqueries)
                )?;
                self.optional(on.as_ref(), bound_on.as_ref(), subqueries)
                    .await
            }
            (
                SourcePlan::Values { rows, .. },
                SourcePlan::Values {
                    rows: bound_rows, ..
                },
            ) => {
                for (lowered, bound) in pairs(rows, bound_rows, "VALUES list")? {
                    self.scalars(lowered, bound, subqueries).await?;
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
                self.routine(name, binding.as_ref()).await?;
                self.scalars(args, bound_args, subqueries).await
            }
            (
                SourcePlan::FunctionGroup { functions, .. },
                SourcePlan::FunctionGroup {
                    functions: bound_functions,
                    ..
                },
            ) => {
                for (lowered, bound) in pairs(functions, bound_functions, "ROWS FROM list")? {
                    self.routine(&bound.name, bound.binding.as_ref()).await?;
                    self.scalars(&lowered.args, &bound.args, subqueries).await?;
                }
                Ok(())
            }
            (SourcePlan::Subquery { body, .. }, SourcePlan::Subquery { body: bound, .. }) => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.query(body, bound, false)
                )
            }
            _ => Err(shape_error("FROM item")),
        }
    }

    async fn optional(
        &mut self,
        lowered: Option<&ScalarExpr>,
        bound: Option<&ScalarExpr>,
        subqueries: Subqueries<'_>,
    ) -> Result<(), SQLError> {
        if let Some((lowered, bound)) = optional_pair(lowered, bound, "clause")? {
            crate::catalog::work::nested!(
                W::MAY_SUSPEND,
                self.control,
                self.scalar(lowered, bound, subqueries)
            )?;
        }
        Ok(())
    }

    async fn scalars(
        &mut self,
        lowered: &[ScalarExpr],
        bound: &[ScalarExpr],
        subqueries: Subqueries<'_>,
    ) -> Result<(), SQLError> {
        for (lowered, bound) in pairs(lowered, bound, "expression list")? {
            self.scalar(lowered, bound, subqueries).await?;
        }
        Ok(())
    }

    async fn subquery(
        &mut self,
        index: usize,
        bound_index: usize,
        subqueries: Subqueries<'_>,
    ) -> Result<(), SQLError> {
        match (subqueries.0.get(index), subqueries.1.get(bound_index)) {
            (Some(lowered), Some(bound)) => crate::catalog::work::nested!(
                W::MAY_SUSPEND,
                self.control,
                self.query(lowered, bound, false)
            ),
            _ => Err(SQLError::Internal(format!(
                "stored syntax cannot resolve subquery slot {index}"
            ))),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one syntax-order walk covers every scalar variant"
    )]
    async fn scalar(
        &mut self,
        lowered: &ScalarExpr,
        bound: &ScalarExpr,
        subqueries: Subqueries<'_>,
    ) -> Result<(), SQLError> {
        self.checkpoint().await?;
        let mut bound = bound;
        while let ScalarExpr::Collated { expr, .. } = bound {
            bound = expr;
        }
        if let Some(clause) = crate::catalog::collations::expression::scalar_clause(lowered)? {
            let mut operand = bound;
            while let ScalarExpr::Cast { expr, .. } = operand {
                operand = expr;
            }
            if crate::catalog::collations::expression::scalar_clause(operand)?.is_none() {
                self.plain(ValueSite::DiscardCollation).await?;
                return crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalar(clause.argument, bound, subqueries)
                );
            }
        }
        if crate::type_resolution::row_distinct::expanded(lowered, bound) {
            let mut rewritten = lowered.clone();
            crate::type_resolution::row_distinct::rewrite_scalar(&mut rewritten)?;
            self.plain(ValueSite::RowDistinct).await?;
            return crate::catalog::work::nested!(
                W::MAY_SUSPEND,
                self.control,
                self.scalar(&rewritten, bound, subqueries)
            );
        }
        if let ScalarExpr::CompositeRow { items, binding, .. } = bound {
            let mut source = match lowered {
                ScalarExpr::Cast { expr, .. } => expr.as_ref(),
                expression => expression,
            };
            while let ScalarExpr::Cast { expr, ty, .. } = source {
                if crate::ColumnType::from_sql_name(ty).ok() != Some(crate::ColumnType::Record) {
                    break;
                }
                source = expr;
            }
            if let ScalarExpr::Row(original)
            | ScalarExpr::CompositeRow {
                items: original, ..
            } = source
            {
                self.composite(binding).await?;
                return crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalars(original, items, subqueries)
                );
            }
        }
        if let Some(shape) = crate::type_resolution::membership::stored_shape(lowered, bound) {
            let ScalarExpr::InList {
                expr,
                list,
                negated,
            } = lowered
            else {
                unreachable!()
            };
            self.membership(&shape.array_items).await?;
            let rewritten = crate::type_resolution::membership::rewrite(
                expr.clone(),
                list.clone(),
                *negated,
                &shape,
                &uqa_core::memory::ProductionControl::uncontrolled(),
            )?;
            return crate::catalog::work::nested!(
                W::MAY_SUSPEND,
                self.control,
                self.scalar(&rewritten, bound, subqueries)
            );
        }
        // Binding wraps an operand in the casts its operator needs, outside the casts the syntax writes.
        for _ in 0..cast_depth(bound).saturating_sub(cast_depth(lowered)) {
            let ScalarExpr::Cast { expr, ty, .. } = bound else {
                return Err(shape_error("relabel"));
            };
            self.text(ty, ValueSite::Relabel).await?;
            bound = expr;
        }
        match lowered {
            ScalarExpr::Literal(Value::Str(_) | Value::Null) => {
                match bound {
                    ScalarExpr::TypedLiteral { value, ty, .. } => self.constant(value, ty).await?,
                    ScalarExpr::Literal(Value::Str(_) | Value::Null) => {
                        self.plain(ValueSite::Literal).await?;
                    }
                    _ => return Err(shape_error("literal")),
                }
                return Ok(());
            }
            ScalarExpr::Cast { .. } => {
                let ScalarExpr::Cast { ty, .. } = bound else {
                    return Err(shape_error("cast"));
                };
                self.text(ty, ValueSite::Cast).await?;
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
                self.plain(ValueSite::FunctionOrder(*bound)).await?;
            }
            _ => self.plain(ValueSite::Node).await?,
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
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalars(args, bound_args, subqueries)
                )?;
                for (lowered, bound) in pairs(order_by, bound_order, "aggregate ORDER BY")? {
                    crate::catalog::work::nested!(
                        W::MAY_SUSPEND,
                        self.control,
                        self.scalar(&lowered.expr, &bound.expr, subqueries)
                    )?;
                }
                self.optional(filter.as_deref(), bound_filter.as_deref(), subqueries)
                    .await?;
                self.routine(name, binding.as_ref()).await?;
                Ok(())
            }
            (ScalarExpr::Array(items), ScalarExpr::Array(bound))
            | (ScalarExpr::Row(items), ScalarExpr::Row(bound))
            | (ScalarExpr::And(items), ScalarExpr::And(bound))
            | (ScalarExpr::Or(items), ScalarExpr::Or(bound)) => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalars(items, bound, subqueries)
                )
            }
            (
                ScalarExpr::Binary { lhs, rhs, .. },
                ScalarExpr::Binary {
                    lhs: bound_lhs,
                    rhs: bound_rhs,
                    ..
                },
            ) => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalar(lhs, bound_lhs, subqueries)
                )?;
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalar(rhs, bound_rhs, subqueries)
                )
            }
            (ScalarExpr::UnaryMinus(inner), ScalarExpr::UnaryMinus(bound))
            | (ScalarExpr::Not(inner), ScalarExpr::Not(bound))
            | (ScalarExpr::IsNull { expr: inner, .. }, ScalarExpr::IsNull { expr: bound, .. })
            | (ScalarExpr::Cast { expr: inner, .. }, ScalarExpr::Cast { expr: bound, .. }) => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalar(inner, bound, subqueries)
                )
            }
            (
                ScalarExpr::Between { expr, low, high },
                ScalarExpr::Between {
                    expr: bound_expr,
                    low: bound_low,
                    high: bound_high,
                },
            ) => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalar(expr, bound_expr, subqueries)
                )?;
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalar(low, bound_low, subqueries)
                )?;
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalar(high, bound_high, subqueries)
                )
            }
            (
                ScalarExpr::InList { expr, list, .. },
                ScalarExpr::InList {
                    expr: bound_expr,
                    list: bound_list,
                    ..
                },
            ) => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalar(expr, bound_expr, subqueries)
                )?;
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalars(list, bound_list, subqueries)
                )
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
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalars(args, bound_args, subqueries)
                )?;
                if let Some((lowered, bound)) =
                    optional_pair(filter.as_deref(), bound_filter.as_deref(), "window FILTER")?
                {
                    crate::catalog::work::nested!(
                        W::MAY_SUSPEND,
                        self.control,
                        self.scalar(lowered, bound, subqueries)
                    )?;
                }
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalars(&spec.partition_by, &bound_spec.partition_by, subqueries)
                )?;
                for (lowered, bound) in
                    pairs(&spec.order_by, &bound_spec.order_by, "window ORDER BY")?
                {
                    crate::catalog::work::nested!(
                        W::MAY_SUSPEND,
                        self.control,
                        self.scalar(&lowered.expr, &bound.expr, subqueries)
                    )?;
                }
                if let Some((lowered, bound)) = optional_pair(
                    spec.frame.as_ref(),
                    bound_spec.frame.as_ref(),
                    "window frame",
                )? {
                    self.frame_bound(&lowered.start, &bound.start, subqueries)
                        .await?;
                    self.frame_bound(&lowered.end, &bound.end, subqueries)
                        .await?;
                }
                self.routine(name, None).await?;
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
                self.optional(base.as_deref(), bound_base.as_deref(), subqueries)
                    .await?;
                for ((condition, result), (bound_condition, bound_result)) in
                    pairs(when, bound_when, "CASE arms")?
                {
                    crate::catalog::work::nested!(
                        W::MAY_SUSPEND,
                        self.control,
                        self.scalar(condition, bound_condition, subqueries)
                    )?;
                    crate::catalog::work::nested!(
                        W::MAY_SUSPEND,
                        self.control,
                        self.scalar(result, bound_result, subqueries)
                    )?;
                }
                self.optional(else_branch.as_deref(), bound_else.as_deref(), subqueries)
                    .await
            }
            (ScalarExpr::ScalarSubquery(index), ScalarExpr::ScalarSubquery(bound))
            | (
                ScalarExpr::Exists {
                    subquery: index, ..
                },
                ScalarExpr::Exists {
                    subquery: bound, ..
                },
            ) => self.subquery(*index, *bound, subqueries).await,
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
            ) => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalar(expr, bound_expr, subqueries)
                )?;
                self.subquery(*index, *bound, subqueries).await
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

    async fn frame_bound(
        &mut self,
        lowered: &ScalarFrameBound,
        bound: &ScalarFrameBound,
        subqueries: Subqueries<'_>,
    ) -> Result<(), SQLError> {
        match (lowered, bound) {
            (ScalarFrameBound::Preceding(lowered), ScalarFrameBound::Preceding(bound))
            | (ScalarFrameBound::Following(lowered), ScalarFrameBound::Following(bound)) => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.scalar(lowered, bound, subqueries)
                )
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
