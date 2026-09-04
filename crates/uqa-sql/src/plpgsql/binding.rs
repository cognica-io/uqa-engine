//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Variable binding across expressions, queries, and statements.

use super::{Expr, FromClause, MergeWhen, Projection, Result, SelectStmt, Statement, Value, CTE};
use crate::ast::InternalColumnRef;

/// Runtime datum value together with the concrete SQL type declared by PL/pgSQL. The type is optional for composite fields and pseudo-types whose runtime carrier already identifies their category.
#[derive(Debug, Clone)]
pub struct ResolvedVariable {
    pub value: Value,
    pub declared_type: Option<String>,
}

impl ResolvedVariable {
    #[must_use]
    pub fn untyped(value: Value) -> Self {
        Self {
            value,
            declared_type: None,
        }
    }

    fn into_expression(self) -> Expr {
        match self.declared_type {
            Some(ty) => Expr::Cast {
                expr: Box::new(Expr::Literal(self.value)),
                ty,
            },
            None => Expr::Literal(self.value),
        }
    }
}

/// Resolves routine variables while a compiled expression / statement
/// is being specialized for one execution.
pub trait VariableResolver {
    /// Current value of an unqualified name. `Ok(None)` leaves the
    /// column reference for the engine to resolve.
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>>;
    /// Current value of `qualifier.column` (record field access).
    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>>;
    /// Value of a positional `$n` reference (function arguments).
    fn resolve_param(&mut self, index: usize) -> Result<Option<ResolvedVariable>>;

    /// Optional expression-level rewrite hook. The default preserves the variable-substitution behavior used by PL/pgSQL while allowing catalog lifecycle code to rewrite a reference without fabricating a literal value.
    fn rewrite_name(&mut self, name: &str) -> Result<Option<Expr>> {
        Ok(self
            .resolve_name(name)?
            .map(ResolvedVariable::into_expression))
    }

    /// Expression-level counterpart of [`Self::resolve_qualified`].
    fn rewrite_qualified(&mut self, qualifier: &str, column: &str) -> Result<Option<Expr>> {
        Ok(self
            .resolve_qualified(qualifier, column)?
            .map(ResolvedVariable::into_expression))
    }

    /// Expand `qualifier.*` when the containing SQL construct treats it as a
    /// list item. Scalar binding deliberately does not call this hook because
    /// `PostgreSQL` treats the same syntax as a composite whole-row value in
    /// scalar contexts.
    fn rewrite_qualified_star(&mut self, _qualifier: &str) -> Result<Option<Vec<Expr>>> {
        Ok(None)
    }

    /// Resolve `qualifier.*` when it appears in a scalar context and therefore denotes one composite whole-row value rather than a projection list.
    fn rewrite_qualified_whole_row(&mut self, _qualifier: &str) -> Result<Option<Expr>> {
        Ok(None)
    }

    /// Expression-level counterpart of [`Self::resolve_param`].
    fn rewrite_param(&mut self, index: usize) -> Result<Option<Expr>> {
        Ok(self
            .resolve_param(index)?
            .map(ResolvedVariable::into_expression))
    }

    /// Observe or replace an executor-only structural column reference. SQL
    /// variable resolvers normally leave these untouched.
    fn rewrite_internal(&mut self, _column: InternalColumnRef) -> Result<Option<Expr>> {
        Ok(None)
    }
}

/// Rewrite an expression, substituting resolvable variable references
/// with literals. References the resolver declines stay untouched.
#[expect(
    clippy::too_many_lines,
    reason = "PL/pgSQL lowering preserves parser order and datum validation"
)]
pub fn bind_expr(expr: &Expr, r: &mut dyn VariableResolver) -> Result<Expr> {
    Ok(match expr {
        Expr::Column(name) => match r.rewrite_name(name)? {
            Some(value) => value,
            None => expr.clone(),
        },
        Expr::QualifiedColumn {
            qualifier, column, ..
        } => match r.rewrite_qualified(qualifier, column)? {
            Some(value) => value,
            None => expr.clone(),
        },
        Expr::Param(index) => match r.rewrite_param(*index)? {
            Some(value) => value,
            None => expr.clone(),
        },
        Expr::InternalColumn(column) => match r.rewrite_internal(*column)? {
            Some(value) => value,
            None => expr.clone(),
        },
        Expr::QualifiedStar(qualifier) => r
            .rewrite_qualified_whole_row(qualifier)?
            .unwrap_or_else(|| expr.clone()),
        Expr::Default | Expr::Literal(_) | Expr::Star => expr.clone(),
        Expr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => Expr::Func {
            name: name.clone(),
            binding: binding.clone(),
            args: bind_exprs(args, r)?,
            distinct: *distinct,
            order_by: bind_order_by(order_by, r)?,
            filter: match filter {
                Some(f) => Some(Box::new(bind_expr(f, r)?)),
                None => None,
            },
        },
        Expr::Array(items) => Expr::Array(bind_exprs(items, r)?),
        Expr::Row(items) => Expr::Row(bind_exprs(items, r)?),
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: Box::new(bind_expr(lhs, r)?),
            rhs: Box::new(bind_expr(rhs, r)?),
        },
        Expr::UnaryMinus(inner) => Expr::UnaryMinus(Box::new(bind_expr(inner, r)?)),
        Expr::Not(inner) => Expr::Not(Box::new(bind_expr(inner, r)?)),
        Expr::And(items) => Expr::And(bind_exprs(items, r)?),
        Expr::Or(items) => Expr::Or(bind_exprs(items, r)?),
        Expr::IsNull { expr, negated } => Expr::IsNull {
            expr: Box::new(bind_expr(expr, r)?),
            negated: *negated,
        },
        Expr::Between { expr, low, high } => Expr::Between {
            expr: Box::new(bind_expr(expr, r)?),
            low: Box::new(bind_expr(low, r)?),
            high: Box::new(bind_expr(high, r)?),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(bind_expr(expr, r)?),
            list: bind_exprs(list, r)?,
            negated: *negated,
        },
        Expr::WindowCall { name, args, spec } => Expr::WindowCall {
            name: name.clone(),
            args: bind_exprs(args, r)?,
            spec: crate::ast::WindowSpec {
                reference: spec.reference.clone(),
                partition_by: bind_exprs(&spec.partition_by, r)?,
                order_by: bind_order_by(&spec.order_by, r)?,
                frame: spec.frame.clone(),
            },
        },
        Expr::Case {
            base,
            when,
            else_branch,
        } => Expr::Case {
            base: match base {
                Some(b) => Some(Box::new(bind_expr(b, r)?)),
                None => None,
            },
            when: when
                .iter()
                .map(|(c, v)| Ok((bind_expr(c, r)?, bind_expr(v, r)?)))
                .collect::<Result<Vec<_>>>()?,
            else_branch: match else_branch {
                Some(e) => Some(Box::new(bind_expr(e, r)?)),
                None => None,
            },
        },
        Expr::Cast { expr, ty } => Expr::Cast {
            expr: Box::new(bind_expr(expr, r)?),
            ty: ty.clone(),
        },
        Expr::ScalarSubquery(body) => Expr::ScalarSubquery(Box::new(bind_select(body, r)?)),
        Expr::Exists { body, negated } => Expr::Exists {
            body: Box::new(bind_select(body, r)?),
            negated: *negated,
        },
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => Expr::InSubquery {
            expr: Box::new(bind_expr(expr, r)?),
            body: Box::new(bind_select(body, r)?),
            negated: *negated,
        },
    })
}

pub(super) fn bind_exprs(exprs: &[Expr], r: &mut dyn VariableResolver) -> Result<Vec<Expr>> {
    exprs.iter().map(|e| bind_expr(e, r)).collect()
}

pub(super) fn bind_opt_expr(
    expr: Option<&Expr>,
    r: &mut dyn VariableResolver,
) -> Result<Option<Expr>> {
    match expr {
        Some(e) => Ok(Some(bind_expr(e, r)?)),
        None => Ok(None),
    }
}

pub(super) fn bind_order_by(
    items: &[crate::ast::OrderBy],
    r: &mut dyn VariableResolver,
) -> Result<Vec<crate::ast::OrderBy>> {
    items
        .iter()
        .map(|o| {
            Ok(crate::ast::OrderBy {
                expr: bind_expr(&o.expr, r)?,
                descending: o.descending,
                nulls: o.nulls,
            })
        })
        .collect()
}

pub(super) fn bind_projections(
    items: &[Projection],
    r: &mut dyn VariableResolver,
) -> Result<Vec<Projection>> {
    items
        .iter()
        .map(|p| {
            Ok(Projection {
                expr: bind_expr(&p.expr, r)?,
                alias: p.alias.clone(),
            })
        })
        .collect()
}

pub(super) fn bind_assignments(
    items: &[(String, Expr)],
    r: &mut dyn VariableResolver,
) -> Result<Vec<(String, Expr)>> {
    items
        .iter()
        .map(|(name, e)| Ok((name.clone(), bind_expr(e, r)?)))
        .collect()
}

pub(super) fn bind_ctes(items: &[CTE], r: &mut dyn VariableResolver) -> Result<Vec<CTE>> {
    items
        .iter()
        .map(|cte| {
            Ok(CTE {
                name: cte.name.clone(),
                columns: cte.columns.clone(),
                recursive: cte.recursive,
                materialization: cte.materialization,
                search: cte.search.clone(),
                cycle: cte
                    .cycle
                    .as_ref()
                    .map(|cycle| -> Result<crate::ast::CteCycleClause> {
                        Ok(crate::ast::CteCycleClause {
                            columns: cycle.columns.clone(),
                            mark_column: cycle.mark_column.clone(),
                            mark_value: bind_expr(&cycle.mark_value, r)?,
                            mark_default: bind_expr(&cycle.mark_default, r)?,
                            path_column: cycle.path_column.clone(),
                        })
                    })
                    .transpose()?,
                query: Box::new(bind_select(&cte.query, r)?),
            })
        })
        .collect()
}

pub(super) fn bind_rows(
    rows: &[Vec<Expr>],
    r: &mut dyn VariableResolver,
) -> Result<Vec<Vec<Expr>>> {
    rows.iter().map(|row| bind_exprs(row, r)).collect()
}

/// Rewrite a `SELECT` body, substituting resolvable variables.
pub fn bind_select(stmt: &SelectStmt, r: &mut dyn VariableResolver) -> Result<SelectStmt> {
    Ok(SelectStmt {
        projections: bind_projections(&stmt.projections, r)?,
        values: bind_rows(&stmt.values, r)?,
        from: match stmt.from.as_ref() {
            Some(f) => Some(bind_from(f, r)?),
            None => None,
        },
        r#where: bind_opt_expr(stmt.r#where.as_ref(), r)?,
        group_by: bind_exprs(&stmt.group_by, r)?,
        grouping_sets: stmt
            .grouping_sets
            .iter()
            .map(|set| bind_exprs(set, r))
            .collect::<Result<Vec<_>>>()?,
        group_distinct: stmt.group_distinct,
        having: bind_opt_expr(stmt.having.as_ref(), r)?,
        order_by: bind_order_by(&stmt.order_by, r)?,
        limit: bind_opt_expr(stmt.limit.as_ref(), r)?,
        with_ties: stmt.with_ties,
        offset: bind_opt_expr(stmt.offset.as_ref(), r)?,
        with: bind_ctes(&stmt.with, r)?,
        set_op: match stmt.set_op.as_ref() {
            Some(op) => Some(Box::new(crate::ast::SetOp {
                kind: op.kind,
                all: op.all,
                left: op
                    .left
                    .as_ref()
                    .map(|left| bind_select(left, r).map(Box::new))
                    .transpose()?,
                right: bind_select(&op.right, r)?,
                combined_order_by: bind_order_by(&op.combined_order_by, r)?,
                combined_limit: bind_opt_expr(op.combined_limit.as_ref(), r)?,
                combined_with_ties: op.combined_with_ties,
                combined_offset: bind_opt_expr(op.combined_offset.as_ref(), r)?,
            })),
            None => None,
        },
        distinct: stmt.distinct,
        distinct_on: bind_exprs(&stmt.distinct_on, r)?,
        locking: stmt.locking.clone(),
    })
}

pub(super) fn bind_from(from: &FromClause, r: &mut dyn VariableResolver) -> Result<FromClause> {
    Ok(match from {
        FromClause::Table { .. } => from.clone(),
        FromClause::Join {
            left,
            right,
            kind,
            on,
            using,
            natural,
            alias,
            column_aliases,
            lateral,
        } => FromClause::Join {
            left: Box::new(bind_from(left, r)?),
            right: Box::new(bind_from(right, r)?),
            kind: *kind,
            on: bind_opt_expr(on.as_ref(), r)?,
            using: using.clone(),
            natural: *natural,
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
            lateral: *lateral,
        },
        FromClause::Values {
            rows,
            alias,
            column_aliases,
            internal_relation,
            internal_column_types,
        } => FromClause::Values {
            rows: bind_rows(rows, r)?,
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
            internal_relation: *internal_relation,
            internal_column_types: internal_column_types.clone(),
        },
        FromClause::Function {
            name,
            binding,
            output_name,
            relations,
            args,
            alias,
            column_aliases,
            ordinality,
            column_types,
        } => FromClause::Function {
            name: name.clone(),
            binding: binding.clone(),
            output_name: output_name.clone(),
            relations: relations.clone(),
            args: bind_exprs(args, r)?,
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
            ordinality: *ordinality,
            column_types: column_types.clone(),
        },
        FromClause::FunctionGroup {
            functions,
            alias,
            column_aliases,
            ordinality,
        } => FromClause::FunctionGroup {
            functions: functions
                .iter()
                .map(|function| {
                    Ok(crate::ast::TableFunction {
                        name: function.name.clone(),
                        binding: function.binding.clone(),
                        output_name: function.output_name.clone(),
                        relations: function.relations.clone(),
                        args: bind_exprs(&function.args, r)?,
                        column_aliases: function.column_aliases.clone(),
                        column_types: function.column_types.clone(),
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
            ordinality: *ordinality,
        },
        FromClause::Subquery {
            body,
            alias,
            column_aliases,
        } => FromClause::Subquery {
            body: Box::new(bind_select(body, r)?),
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
        },
    })
}

/// Rewrite a full statement, substituting resolvable variables in
/// every expression position. Statements without expression payloads
/// pass through unchanged.
#[expect(
    clippy::too_many_lines,
    reason = "PL/pgSQL lowering preserves parser order and datum validation"
)]
pub fn bind_statement(stmt: &Statement, r: &mut dyn VariableResolver) -> Result<Statement> {
    Ok(match stmt {
        Statement::Select(body) => Statement::Select(Box::new(bind_select(body, r)?)),
        Statement::Insert(insert) => {
            let mut out = insert.clone();
            out.with = bind_ctes(&insert.with, r)?;
            out.rows = bind_rows(&insert.rows, r)?;
            out.select_source = match insert.select_source.as_ref() {
                Some(body) => Some(Box::new(bind_select(body, r)?)),
                None => None,
            };
            out.on_conflict = match insert.on_conflict.as_ref() {
                Some(oc) => Some(crate::ast::OnConflict {
                    conflict_columns: oc.conflict_columns.clone(),
                    action: match &oc.action {
                        crate::ast::OnConflictAction::Nothing => {
                            crate::ast::OnConflictAction::Nothing
                        }
                        crate::ast::OnConflictAction::Update {
                            assignments,
                            r#where,
                        } => crate::ast::OnConflictAction::Update {
                            assignments: bind_assignments(assignments, r)?,
                            r#where: bind_opt_expr(r#where.as_ref(), r)?,
                        },
                    },
                }),
                None => None,
            };
            out.returning = bind_projections(&insert.returning, r)?;
            Statement::Insert(out)
        }
        Statement::Update(update) => {
            let mut out = update.clone();
            out.assignments = bind_assignments(&update.assignments, r)?;
            out.r#where = bind_opt_expr(update.r#where.as_ref(), r)?;
            out.with = bind_ctes(&update.with, r)?;
            out.from = match update.from.as_ref() {
                Some(f) => Some(bind_from(f, r)?),
                None => None,
            };
            out.returning = bind_projections(&update.returning, r)?;
            Statement::Update(out)
        }
        Statement::Delete(delete) => {
            let mut out = delete.clone();
            out.r#where = bind_opt_expr(delete.r#where.as_ref(), r)?;
            out.with = bind_ctes(&delete.with, r)?;
            out.using = match delete.using.as_ref() {
                Some(f) => Some(bind_from(f, r)?),
                None => None,
            };
            out.returning = bind_projections(&delete.returning, r)?;
            Statement::Delete(out)
        }
        Statement::Values { rows } => Statement::Values {
            rows: bind_rows(rows, r)?,
        },
        Statement::CreateTableAs {
            name,
            if_not_exists,
            column_names,
            with_no_data,
            persistence,
            on_commit,
            body,
        } => Statement::CreateTableAs {
            name: name.clone(),
            if_not_exists: *if_not_exists,
            column_names: column_names.clone(),
            with_no_data: *with_no_data,
            persistence: *persistence,
            on_commit: *on_commit,
            body: Box::new(bind_select(body, r)?),
        },
        Statement::CreateMaterializedView {
            name,
            column_names,
            if_not_exists,
            with_no_data,
            options,
            body,
        } => Statement::CreateMaterializedView {
            name: name.clone(),
            column_names: column_names.clone(),
            if_not_exists: *if_not_exists,
            with_no_data: *with_no_data,
            options: options.clone(),
            body: Box::new(bind_select(body, r)?),
        },
        Statement::Explain {
            analyze,
            verbose,
            format,
            body,
        } => Statement::Explain {
            analyze: *analyze,
            verbose: *verbose,
            format: format.clone(),
            body: Box::new(bind_statement(body, r)?),
        },
        Statement::DeclareCursor(cursor) => {
            let mut out = cursor.clone();
            out.query = Box::new(bind_select(&cursor.query, r)?);
            Statement::DeclareCursor(out)
        }
        Statement::Merge(merge) => {
            let mut out = merge.clone();
            out.source = bind_from(&merge.source, r)?;
            out.join_condition = bind_expr(&merge.join_condition, r)?;
            out.when_clauses = merge
                .when_clauses
                .iter()
                .map(|w| bind_merge_when(w, r))
                .collect::<Result<Vec<_>>>()?;
            out.returning = bind_projections(&merge.returning, r)?;
            Statement::Merge(out)
        }
        Statement::Call { name, args } => Statement::Call {
            name: name.clone(),
            args: bind_exprs(args, r)?,
        },
        other => other.clone(),
    })
}

pub(super) fn bind_merge_when(when: &MergeWhen, r: &mut dyn VariableResolver) -> Result<MergeWhen> {
    Ok(match when {
        MergeWhen::UpdateMatched {
            condition,
            assignments,
        } => MergeWhen::UpdateMatched {
            condition: bind_opt_expr(condition.as_ref(), r)?,
            assignments: bind_assignments(assignments, r)?,
        },
        MergeWhen::DeleteMatched { condition } => MergeWhen::DeleteMatched {
            condition: bind_opt_expr(condition.as_ref(), r)?,
        },
        MergeWhen::UpdateNotMatchedBySource {
            condition,
            assignments,
        } => MergeWhen::UpdateNotMatchedBySource {
            condition: bind_opt_expr(condition.as_ref(), r)?,
            assignments: bind_assignments(assignments, r)?,
        },
        MergeWhen::DeleteNotMatchedBySource { condition } => MergeWhen::DeleteNotMatchedBySource {
            condition: bind_opt_expr(condition.as_ref(), r)?,
        },
        MergeWhen::InsertNotMatched {
            condition,
            columns,
            values,
        } => MergeWhen::InsertNotMatched {
            condition: bind_opt_expr(condition.as_ref(), r)?,
            columns: columns.clone(),
            values: bind_exprs(values, r)?,
        },
        MergeWhen::NothingMatched { condition } => MergeWhen::NothingMatched {
            condition: bind_opt_expr(condition.as_ref(), r)?,
        },
        MergeWhen::NothingNotMatched { condition } => MergeWhen::NothingNotMatched {
            condition: bind_opt_expr(condition.as_ref(), r)?,
        },
        MergeWhen::NothingNotMatchedBySource { condition } => {
            MergeWhen::NothingNotMatchedBySource {
                condition: bind_opt_expr(condition.as_ref(), r)?,
            }
        }
    })
}
