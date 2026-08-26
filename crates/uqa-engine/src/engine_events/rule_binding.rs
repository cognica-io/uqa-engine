//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-scope-aware binding for rewrite-rule OLD/NEW pseudo-relations.

use std::collections::BTreeSet;

use uqa_sql::ast::{
    Expr, FrameBound, FromClause, InsertStmt, OnConflictAction, OrderBy, Projection, SelectStmt,
    Statement, UpdateStmt, CTE,
};
use uqa_sql::plpgsql::{bind_statement, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

#[derive(Default)]
struct RuleRowReferenceDetector {
    qualifier: Option<String>,
}

impl VariableResolver for RuleRowReferenceDetector {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if self.qualifier.is_none()
            && (qualifier.eq_ignore_ascii_case("old") || qualifier.eq_ignore_ascii_case("new"))
        {
            self.qualifier = Some(qualifier.to_ascii_lowercase());
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
}

pub(crate) fn rule_action_has_set_operation(action: &Statement) -> bool {
    match action {
        Statement::Select(select) => select.set_op.is_some(),
        Statement::Insert(insert) => insert
            .select_source
            .as_ref()
            .is_some_and(|select| select.set_op.is_some()),
        _ => false,
    }
}

pub(crate) fn first_rule_row_reference_in_expr(
    expr: &Expr,
    shadowed: &BTreeSet<String>,
) -> Option<String> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_expr_scoped(expr, &mut detector, shadowed);
    detector.qualifier
}

pub(crate) fn first_rule_row_reference_in_select(select: &SelectStmt) -> Option<String> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_select_scoped(select, &mut detector);
    detector.qualifier
}

pub(crate) fn rule_expr_references_row(expr: &Expr) -> bool {
    first_rule_row_reference_in_expr(expr, &BTreeSet::new()).is_some()
}

pub(crate) fn rule_statement_references_row(
    statement: &Statement,
    action_columns: &BTreeSet<String>,
) -> Result<bool, SQLError> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_action(statement, action_columns, &mut detector)?;
    Ok(detector.qualifier.is_some())
}

/// Bind an action body's event-row OLD/NEW references while preserving DML
/// RETURNING OLD/NEW references for the action target's own row images.
pub(crate) fn bind_rule_action(
    action: &Statement,
    action_columns: &BTreeSet<String>,
    resolver: &mut dyn VariableResolver,
) -> Result<Statement, SQLError> {
    let returning = match action {
        Statement::Insert(statement) => &statement.returning,
        Statement::Update(statement) => &statement.returning,
        Statement::Delete(statement) => &statement.returning,
        _ => return bind_rule_statement_body(action, resolver, &BTreeSet::new()),
    };
    let mut body = action.clone();
    let aliases = match &mut body {
        Statement::Insert(statement) => {
            statement.returning.clear();
            statement.returning_aliases.clone()
        }
        Statement::Update(statement) => {
            statement.returning.clear();
            statement.returning_aliases.clone()
        }
        Statement::Delete(statement) => {
            statement.returning.clear();
            statement.returning_aliases.clone()
        }
        _ => unreachable!("DML rule action changed statement kind"),
    };
    let mut bound = bind_rule_statement_body(&body, resolver, &BTreeSet::new())?;
    if returning.is_empty() {
        return Ok(bound);
    }
    let mut returning_resolver = RuleActionReturningResolver {
        action_columns,
        aliases: &aliases,
    };
    let returning = returning
        .iter()
        .map(|projection| {
            Ok(Projection {
                expr: bind_rule_expr_scoped(
                    &projection.expr,
                    &mut returning_resolver,
                    &BTreeSet::new(),
                )?,
                alias: projection.alias.clone(),
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    match &mut bound {
        Statement::Insert(statement) => statement.returning = returning,
        Statement::Update(statement) => statement.returning = returning,
        Statement::Delete(statement) => statement.returning = returning,
        _ => unreachable!("bound DML rule action changed statement kind"),
    }
    Ok(bound)
}

/// Bind one expression while masking relation qualifiers supplied by the SQL scope around it. Nested query scopes add their own visible aliases.
pub(crate) fn bind_rule_expr_scoped(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
) -> Result<Expr, SQLError> {
    Ok(match expr {
        Expr::Column(name) => resolver.rewrite_name(name)?.unwrap_or_else(|| expr.clone()),
        Expr::QualifiedColumn { qualifier, column } => {
            if qualifier_is_shadowed(shadowed, qualifier) {
                expr.clone()
            } else {
                resolver
                    .rewrite_qualified(qualifier, column)?
                    .unwrap_or_else(|| expr.clone())
            }
        }
        Expr::Param(index) => resolver
            .rewrite_param(*index)?
            .unwrap_or_else(|| expr.clone()),
        Expr::InternalColumn(column) => resolver
            .rewrite_internal(*column)?
            .unwrap_or_else(|| expr.clone()),
        Expr::Default | Expr::Literal(_) | Expr::Star | Expr::QualifiedStar(_) => expr.clone(),
        Expr::Func { .. } => bind_rule_function_expression(expr, resolver, shadowed)?,
        Expr::Array(items) => Expr::Array(bind_exprs(items, resolver, shadowed)?),
        Expr::Row(items) => Expr::Row(bind_exprs(items, resolver, shadowed)?),
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: Box::new(bind_rule_expr_scoped(lhs, resolver, shadowed)?),
            rhs: Box::new(bind_rule_expr_scoped(rhs, resolver, shadowed)?),
        },
        Expr::UnaryMinus(inner) => {
            Expr::UnaryMinus(Box::new(bind_rule_expr_scoped(inner, resolver, shadowed)?))
        }
        Expr::Not(inner) => Expr::Not(Box::new(bind_rule_expr_scoped(inner, resolver, shadowed)?)),
        Expr::And(items) => Expr::And(bind_exprs(items, resolver, shadowed)?),
        Expr::Or(items) => Expr::Or(bind_exprs(items, resolver, shadowed)?),
        Expr::IsNull { expr, negated } => Expr::IsNull {
            expr: Box::new(bind_rule_expr_scoped(expr, resolver, shadowed)?),
            negated: *negated,
        },
        Expr::Between { expr, low, high } => Expr::Between {
            expr: Box::new(bind_rule_expr_scoped(expr, resolver, shadowed)?),
            low: Box::new(bind_rule_expr_scoped(low, resolver, shadowed)?),
            high: Box::new(bind_rule_expr_scoped(high, resolver, shadowed)?),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(bind_rule_expr_scoped(expr, resolver, shadowed)?),
            list: bind_exprs(list, resolver, shadowed)?,
            negated: *negated,
        },
        Expr::WindowCall { .. } => bind_rule_window_expression(expr, resolver, shadowed)?,
        Expr::Case { .. } => bind_rule_case_expression(expr, resolver, shadowed)?,
        Expr::Cast { expr, ty } => Expr::Cast {
            expr: Box::new(bind_rule_expr_scoped(expr, resolver, shadowed)?),
            ty: ty.clone(),
        },
        Expr::ScalarSubquery(body) => {
            Expr::ScalarSubquery(Box::new(bind_select_with_scope(body, resolver, shadowed)?))
        }
        Expr::Exists { body, negated } => Expr::Exists {
            body: Box::new(bind_select_with_scope(body, resolver, shadowed)?),
            negated: *negated,
        },
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => Expr::InSubquery {
            expr: Box::new(bind_rule_expr_scoped(expr, resolver, shadowed)?),
            body: Box::new(bind_select_with_scope(body, resolver, shadowed)?),
            negated: *negated,
        },
    })
}

fn bind_rule_function_expression(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
) -> Result<Expr, SQLError> {
    let Expr::Func {
        name,
        binding,
        args,
        distinct,
        order_by,
        filter,
    } = expr
    else {
        unreachable!("function binder received a non-function expression")
    };
    Ok(Expr::Func {
        name: name.clone(),
        binding: binding.clone(),
        args: bind_exprs(args, resolver, shadowed)?,
        distinct: *distinct,
        order_by: bind_orders(order_by, resolver, shadowed)?,
        filter: filter
            .as_deref()
            .map(|expr| bind_rule_expr_scoped(expr, resolver, shadowed).map(Box::new))
            .transpose()?,
    })
}

fn bind_rule_window_expression(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
) -> Result<Expr, SQLError> {
    let Expr::WindowCall { name, args, spec } = expr else {
        unreachable!("window binder received a non-window expression")
    };
    Ok(Expr::WindowCall {
        name: name.clone(),
        args: bind_exprs(args, resolver, shadowed)?,
        spec: uqa_sql::ast::WindowSpec {
            reference: spec.reference.clone(),
            partition_by: bind_exprs(&spec.partition_by, resolver, shadowed)?,
            order_by: bind_orders(&spec.order_by, resolver, shadowed)?,
            frame: spec
                .frame
                .as_ref()
                .map(|frame| -> Result<uqa_sql::ast::WindowFrame, SQLError> {
                    Ok(uqa_sql::ast::WindowFrame {
                        mode: frame.mode,
                        start: bind_frame_bound(&frame.start, resolver, shadowed)?,
                        end: bind_frame_bound(&frame.end, resolver, shadowed)?,
                    })
                })
                .transpose()?,
        },
    })
}

fn bind_rule_case_expression(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
) -> Result<Expr, SQLError> {
    let Expr::Case {
        base,
        when,
        else_branch,
    } = expr
    else {
        unreachable!("CASE binder received a non-CASE expression")
    };
    Ok(Expr::Case {
        base: base
            .as_deref()
            .map(|expr| bind_rule_expr_scoped(expr, resolver, shadowed).map(Box::new))
            .transpose()?,
        when: when
            .iter()
            .map(|(condition, result)| {
                Ok((
                    bind_rule_expr_scoped(condition, resolver, shadowed)?,
                    bind_rule_expr_scoped(result, resolver, shadowed)?,
                ))
            })
            .collect::<Result<Vec<_>, SQLError>>()?,
        else_branch: else_branch
            .as_deref()
            .map(|expr| bind_rule_expr_scoped(expr, resolver, shadowed).map(Box::new))
            .transpose()?,
    })
}

fn bind_rule_select_scoped(
    select: &SelectStmt,
    resolver: &mut dyn VariableResolver,
) -> Result<SelectStmt, SQLError> {
    bind_select_with_scope(select, resolver, &BTreeSet::new())
}

fn bind_rule_statement_body(
    statement: &Statement,
    resolver: &mut dyn VariableResolver,
    inherited: &BTreeSet<String>,
) -> Result<Statement, SQLError> {
    Ok(match statement {
        Statement::Select(select) => Statement::Select(Box::new(bind_select_with_scope(
            select, resolver, inherited,
        )?)),
        Statement::Insert(insert) => Statement::Insert(bind_insert(insert, resolver, inherited)?),
        Statement::Update(update) => Statement::Update(bind_update(update, resolver, inherited)?),
        Statement::Delete(delete) => {
            let mut output = delete.clone();
            output.with = bind_ctes(&delete.with, resolver, inherited)?;
            let mut target_scope = inherited.clone();
            insert_qualifier(&mut target_scope, &delete.target_qualifier);
            output.using = delete
                .using
                .as_ref()
                .map(|source| bind_from(source, resolver, &target_scope))
                .transpose()?;
            let mut expression_scope = target_scope;
            if let Some(source) = &delete.using {
                collect_visible_qualifiers(source, &mut expression_scope);
            }
            output.r#where =
                bind_optional_expr(delete.r#where.as_ref(), resolver, &expression_scope)?;
            output.returning.clear();
            Statement::Delete(output)
        }
        _ => return bind_statement(statement, resolver),
    })
}

fn bind_insert(
    insert: &InsertStmt,
    resolver: &mut dyn VariableResolver,
    inherited: &BTreeSet<String>,
) -> Result<InsertStmt, SQLError> {
    let mut output = insert.clone();
    output.with = bind_ctes(&insert.with, resolver, inherited)?;
    output.rows = insert
        .rows
        .iter()
        .map(|row| bind_exprs(row, resolver, inherited))
        .collect::<Result<Vec<_>, SQLError>>()?;
    output.select_source = insert
        .select_source
        .as_deref()
        .map(|select| bind_select_with_scope(select, resolver, inherited).map(Box::new))
        .transpose()?;
    output.on_conflict = insert
        .on_conflict
        .as_ref()
        .map(|conflict| -> Result<uqa_sql::ast::OnConflict, SQLError> {
            let mut conflict_scope = inherited.clone();
            insert_qualifier(&mut conflict_scope, &insert.target_qualifier);
            Ok(uqa_sql::ast::OnConflict {
                conflict_columns: conflict.conflict_columns.clone(),
                action: match &conflict.action {
                    OnConflictAction::Nothing => OnConflictAction::Nothing,
                    OnConflictAction::Update {
                        assignments,
                        r#where,
                    } => OnConflictAction::Update {
                        assignments: assignments
                            .iter()
                            .map(|(column, expr)| {
                                Ok((
                                    column.clone(),
                                    bind_rule_expr_scoped(expr, resolver, &conflict_scope)?,
                                ))
                            })
                            .collect::<Result<Vec<_>, SQLError>>()?,
                        r#where: bind_optional_expr(r#where.as_ref(), resolver, &conflict_scope)?,
                    },
                },
            })
        })
        .transpose()?;
    output.returning.clear();
    Ok(output)
}

fn bind_update(
    update: &UpdateStmt,
    resolver: &mut dyn VariableResolver,
    inherited: &BTreeSet<String>,
) -> Result<UpdateStmt, SQLError> {
    let mut output = update.clone();
    output.with = bind_ctes(&update.with, resolver, inherited)?;
    let mut target_scope = inherited.clone();
    insert_qualifier(&mut target_scope, &update.target_qualifier);
    output.from = update
        .from
        .as_ref()
        .map(|source| bind_from(source, resolver, &target_scope))
        .transpose()?;
    let mut expression_scope = target_scope;
    if let Some(source) = &update.from {
        collect_visible_qualifiers(source, &mut expression_scope);
    }
    output.assignments = update
        .assignments
        .iter()
        .map(|(column, expr)| {
            Ok((
                column.clone(),
                bind_rule_expr_scoped(expr, resolver, &expression_scope)?,
            ))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    output.r#where = bind_optional_expr(update.r#where.as_ref(), resolver, &expression_scope)?;
    output.returning.clear();
    Ok(output)
}

fn bind_select_with_scope(
    select: &SelectStmt,
    resolver: &mut dyn VariableResolver,
    inherited: &BTreeSet<String>,
) -> Result<SelectStmt, SQLError> {
    let mut scope = inherited.clone();
    if let Some(source) = &select.from {
        collect_visible_qualifiers(source, &mut scope);
    }
    Ok(SelectStmt {
        projections: bind_projections(&select.projections, resolver, &scope)?,
        values: select
            .values
            .iter()
            .map(|row| bind_exprs(row, resolver, &scope))
            .collect::<Result<Vec<_>, SQLError>>()?,
        from: select
            .from
            .as_ref()
            .map(|source| bind_from(source, resolver, inherited))
            .transpose()?,
        r#where: bind_optional_expr(select.r#where.as_ref(), resolver, &scope)?,
        group_by: bind_exprs(&select.group_by, resolver, &scope)?,
        grouping_sets: select
            .grouping_sets
            .iter()
            .map(|set| bind_exprs(set, resolver, &scope))
            .collect::<Result<Vec<_>, SQLError>>()?,
        group_distinct: select.group_distinct,
        having: bind_optional_expr(select.having.as_ref(), resolver, &scope)?,
        order_by: bind_orders(&select.order_by, resolver, &scope)?,
        limit: bind_optional_expr(select.limit.as_ref(), resolver, &scope)?,
        with_ties: select.with_ties,
        offset: bind_optional_expr(select.offset.as_ref(), resolver, &scope)?,
        with: bind_ctes(&select.with, resolver, inherited)?,
        set_op: select
            .set_op
            .as_ref()
            .map(|operation| -> Result<Box<uqa_sql::ast::SetOp>, SQLError> {
                Ok(Box::new(uqa_sql::ast::SetOp {
                    kind: operation.kind,
                    all: operation.all,
                    left: operation
                        .left
                        .as_deref()
                        .map(|left| bind_select_with_scope(left, resolver, inherited).map(Box::new))
                        .transpose()?,
                    right: bind_select_with_scope(&operation.right, resolver, inherited)?,
                    combined_order_by: bind_orders(
                        &operation.combined_order_by,
                        resolver,
                        inherited,
                    )?,
                    combined_limit: bind_optional_expr(
                        operation.combined_limit.as_ref(),
                        resolver,
                        inherited,
                    )?,
                    combined_with_ties: operation.combined_with_ties,
                    combined_offset: bind_optional_expr(
                        operation.combined_offset.as_ref(),
                        resolver,
                        inherited,
                    )?,
                }))
            })
            .transpose()?,
        distinct: select.distinct,
        distinct_on: bind_exprs(&select.distinct_on, resolver, &scope)?,
        locking: select.locking.clone(),
    })
}

fn bind_from(
    from: &FromClause,
    resolver: &mut dyn VariableResolver,
    inherited: &BTreeSet<String>,
) -> Result<FromClause, SQLError> {
    Ok(match from {
        FromClause::Table { .. } => from.clone(),
        FromClause::Join { .. } => bind_join_from(from, resolver, inherited)?,
        FromClause::Values {
            rows,
            alias,
            column_aliases,
            internal_relation,
            internal_column_types,
        } => FromClause::Values {
            rows: rows
                .iter()
                .map(|row| bind_exprs(row, resolver, inherited))
                .collect::<Result<Vec<_>, SQLError>>()?,
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
            internal_relation: *internal_relation,
            internal_column_types: internal_column_types.clone(),
        },
        FromClause::Function {
            name,
            output_name,
            relation,
            args,
            alias,
            column_aliases,
            ordinality,
            column_types,
        } => FromClause::Function {
            name: name.clone(),
            output_name: output_name.clone(),
            relation: relation.clone(),
            args: bind_exprs(args, resolver, inherited)?,
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
                    Ok(uqa_sql::ast::TableFunction {
                        name: function.name.clone(),
                        output_name: function.output_name.clone(),
                        relation: function.relation.clone(),
                        args: bind_exprs(&function.args, resolver, inherited)?,
                        column_aliases: function.column_aliases.clone(),
                        column_types: function.column_types.clone(),
                    })
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
            ordinality: *ordinality,
        },
        FromClause::Subquery {
            body,
            alias,
            column_aliases,
        } => FromClause::Subquery {
            body: Box::new(bind_select_with_scope(body, resolver, inherited)?),
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
        },
    })
}

fn bind_join_from(
    from: &FromClause,
    resolver: &mut dyn VariableResolver,
    inherited: &BTreeSet<String>,
) -> Result<FromClause, SQLError> {
    let FromClause::Join {
        left,
        right,
        kind,
        on,
        using,
        natural,
        alias,
        column_aliases,
        lateral,
    } = from
    else {
        unreachable!("join binder received a non-join source")
    };
    let bound_left = bind_from(left, resolver, inherited)?;
    let mut right_scope = inherited.clone();
    if *lateral {
        collect_visible_qualifiers(left, &mut right_scope);
    }
    let bound_right = bind_from(right, resolver, &right_scope)?;
    let mut on_scope = inherited.clone();
    collect_visible_qualifiers(left, &mut on_scope);
    collect_visible_qualifiers(right, &mut on_scope);
    Ok(FromClause::Join {
        left: Box::new(bound_left),
        right: Box::new(bound_right),
        kind: *kind,
        on: bind_optional_expr(on.as_ref(), resolver, &on_scope)?,
        using: using.clone(),
        natural: *natural,
        alias: alias.clone(),
        column_aliases: column_aliases.clone(),
        lateral: *lateral,
    })
}

fn bind_ctes(
    ctes: &[CTE],
    resolver: &mut dyn VariableResolver,
    inherited: &BTreeSet<String>,
) -> Result<Vec<CTE>, SQLError> {
    ctes.iter()
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
                    .map(|cycle| -> Result<uqa_sql::ast::CteCycleClause, SQLError> {
                        Ok(uqa_sql::ast::CteCycleClause {
                            columns: cycle.columns.clone(),
                            mark_column: cycle.mark_column.clone(),
                            mark_value: bind_rule_expr_scoped(
                                &cycle.mark_value,
                                resolver,
                                inherited,
                            )?,
                            mark_default: bind_rule_expr_scoped(
                                &cycle.mark_default,
                                resolver,
                                inherited,
                            )?,
                            path_column: cycle.path_column.clone(),
                        })
                    })
                    .transpose()?,
                query: Box::new(bind_select_with_scope(&cte.query, resolver, inherited)?),
            })
        })
        .collect()
}

fn bind_frame_bound(
    bound: &FrameBound,
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
) -> Result<FrameBound, SQLError> {
    Ok(match bound {
        FrameBound::Preceding(expr) => {
            FrameBound::Preceding(Box::new(bind_rule_expr_scoped(expr, resolver, shadowed)?))
        }
        FrameBound::Following(expr) => {
            FrameBound::Following(Box::new(bind_rule_expr_scoped(expr, resolver, shadowed)?))
        }
        _ => bound.clone(),
    })
}

fn bind_projections(
    projections: &[Projection],
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
) -> Result<Vec<Projection>, SQLError> {
    projections
        .iter()
        .map(|projection| {
            Ok(Projection {
                expr: bind_rule_expr_scoped(&projection.expr, resolver, shadowed)?,
                alias: projection.alias.clone(),
            })
        })
        .collect()
}

fn bind_exprs(
    expressions: &[Expr],
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
) -> Result<Vec<Expr>, SQLError> {
    expressions
        .iter()
        .map(|expr| bind_rule_expr_scoped(expr, resolver, shadowed))
        .collect()
}

fn bind_optional_expr(
    expression: Option<&Expr>,
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
) -> Result<Option<Expr>, SQLError> {
    expression
        .map(|expr| bind_rule_expr_scoped(expr, resolver, shadowed))
        .transpose()
}

fn bind_orders(
    orders: &[OrderBy],
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
) -> Result<Vec<OrderBy>, SQLError> {
    orders
        .iter()
        .map(|order| {
            Ok(OrderBy {
                expr: bind_rule_expr_scoped(&order.expr, resolver, shadowed)?,
                descending: order.descending,
                nulls: order.nulls,
            })
        })
        .collect()
}

fn collect_visible_qualifiers(from: &FromClause, output: &mut BTreeSet<String>) {
    match from {
        FromClause::Table {
            name,
            qualifier,
            alias,
            ..
        } => {
            if let Some(alias) = alias {
                insert_qualifier(output, alias);
            } else {
                insert_qualifier(output, qualifier);
                insert_qualifier(output, name);
                if let Some((_, local)) = name.rsplit_once('.') {
                    insert_qualifier(output, local.trim_matches('"'));
                }
            }
        }
        FromClause::Join {
            left,
            right,
            alias,
            using,
            ..
        } => {
            if let Some(alias) = alias {
                insert_qualifier(output, alias);
            } else {
                collect_visible_qualifiers(left, output);
                collect_visible_qualifiers(right, output);
            }
            if let Some(alias) = using.as_ref().and_then(|using| using.alias.as_ref()) {
                insert_qualifier(output, alias);
            }
        }
        FromClause::Values { alias, .. } | FromClause::Subquery { alias, .. } => {
            if let Some(alias) = alias {
                insert_qualifier(output, alias);
            }
        }
        FromClause::Function {
            output_name, alias, ..
        } => insert_qualifier(output, alias.as_deref().unwrap_or(output_name)),
        FromClause::FunctionGroup {
            functions, alias, ..
        } => {
            if let Some(alias) = alias {
                insert_qualifier(output, alias);
            } else {
                for function in functions {
                    insert_qualifier(output, &function.output_name);
                }
            }
        }
    }
}

fn insert_qualifier(output: &mut BTreeSet<String>, qualifier: &str) {
    output.insert(qualifier.to_ascii_lowercase());
}

fn qualifier_is_shadowed(shadowed: &BTreeSet<String>, qualifier: &str) -> bool {
    shadowed.contains(&qualifier.to_ascii_lowercase())
}

struct RuleActionReturningResolver<'a> {
    action_columns: &'a BTreeSet<String>,
    aliases: &'a uqa_sql::ast::ReturningAliases,
}

impl RuleActionReturningResolver<'_> {
    fn is_action_image_alias(&self, qualifier: &str) -> bool {
        qualifier.eq_ignore_ascii_case(&self.aliases.old)
            || qualifier.eq_ignore_ascii_case(&self.aliases.new)
    }
}

impl VariableResolver for RuleActionReturningResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if self.is_action_image_alias(qualifier) && !self.action_columns.contains(column) {
            return Err(SQLError::UnknownColumn(format!("{qualifier}.{column}")));
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        self.resolve_qualified(qualifier, column)?;
        Ok(None)
    }
}
