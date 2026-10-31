//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered query and source syntax construction for stored rule actions.

use super::{
    bind_expanding_exprs, bind_exprs, bind_optional_expr, bind_orders, bind_projections,
    bind_rule_expr_with_scope, bind_window_spec, collect_visible_scope, RuleBindingContext,
    RuleBindingScope,
};
use crate::{
    ast::{FromClause, SelectStmt, CTE},
    plan::copy::CollectSyntax,
    plpgsql::VariableResolver,
    SQLError,
};

pub(super) fn bind_select_with_scope(
    select: &SelectStmt,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &crate::plan::copy::SyntaxProduction<'_>,
) -> Result<SelectStmt, SQLError> {
    let local_context = context.with_ctes(&select.with)?;
    let mut scope = inherited.copy(&context.control())?;
    if let Some(source) = &select.from {
        collect_visible_scope(source, &local_context, &mut scope)?;
    }
    Ok(SelectStmt {
        windows: bind_windows(
            &select.windows,
            resolver,
            &scope,
            &local_context,
            production,
        )?,
        projections: bind_projections(
            &select.projections,
            resolver,
            &scope,
            &local_context,
            production,
        )?,
        values: select
            .values
            .iter()
            .map(|row| bind_expanding_exprs(row, resolver, &scope, &local_context, production))
            .collect_syntax(production)?,
        from: select
            .from
            .as_ref()
            .map(|source| bind_from(source, resolver, inherited, &local_context, production))
            .transpose()?,
        r#where: bind_optional_expr(
            select.r#where.as_ref(),
            resolver,
            &scope,
            &local_context,
            production,
        )?,
        group_by: bind_exprs(
            &select.group_by,
            resolver,
            &scope,
            &local_context,
            production,
        )?,
        grouping_sets: select
            .grouping_sets
            .iter()
            .map(|set| bind_exprs(set, resolver, &scope, &local_context, production))
            .collect_syntax(production)?,
        group_distinct: select.group_distinct,
        having: bind_optional_expr(
            select.having.as_ref(),
            resolver,
            &scope,
            &local_context,
            production,
        )?,
        order_by: bind_orders(
            &select.order_by,
            resolver,
            &scope,
            &local_context,
            production,
        )?,
        limit: bind_optional_expr(
            select.limit.as_ref(),
            resolver,
            &scope,
            &local_context,
            production,
        )?,
        with_ties: select.with_ties,
        offset: bind_optional_expr(
            select.offset.as_ref(),
            resolver,
            &scope,
            &local_context,
            production,
        )?,
        with: bind_ctes(&select.with, resolver, inherited, context, production)?,
        set_op: select
            .set_op
            .as_ref()
            .map(|operation| {
                bind_set_operation(operation, resolver, inherited, &local_context, production)
            })
            .transpose()?,
        distinct: select.distinct,
        distinct_on: bind_exprs(
            &select.distinct_on,
            resolver,
            &scope,
            &local_context,
            production,
        )?,
        locking: production.copy(&select.locking)?,
    })
}

pub(super) fn bind_from(
    from: &FromClause,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &crate::plan::copy::SyntaxProduction<'_>,
) -> Result<FromClause, SQLError> {
    Ok(match from {
        crate::ast::FromClause::RetainedRows(_) => production.copy(from)?,
        FromClause::Table { .. } => production.copy(from)?,
        FromClause::Join { .. } => bind_join_from(from, resolver, inherited, context, production)?,
        FromClause::Values {
            lateral,
            rows,
            alias,
            column_aliases,
            internal_relation,
            internal_column_types,
            internal_column_collations,
        } => FromClause::Values {
            lateral: *lateral,
            rows: rows
                .iter()
                .map(|row| bind_exprs(row, resolver, inherited, context, production))
                .collect_syntax(production)?,
            alias: production.copy(alias)?,
            column_aliases: production.copy(column_aliases)?,
            internal_relation: *internal_relation,
            internal_column_types: production.copy(internal_column_types)?,
            internal_column_collations: production.copy(internal_column_collations)?,
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
            name: production.copy(name)?,
            binding: production.copy(binding)?,
            output_name: production.copy(output_name)?,
            relations: production.copy(relations)?,
            args: bind_exprs(args, resolver, inherited, context, production)?,
            alias: production.copy(alias)?,
            column_aliases: production.copy(column_aliases)?,
            ordinality: *ordinality,
            column_types: production.copy(column_types)?,
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
                        name: production.copy(&function.name)?,
                        binding: production.copy(&function.binding)?,
                        output_name: production.copy(&function.output_name)?,
                        relations: production.copy(&function.relations)?,
                        args: bind_exprs(&function.args, resolver, inherited, context, production)?,
                        column_aliases: production.copy(&function.column_aliases)?,
                        column_types: production.copy(&function.column_types)?,
                    })
                })
                .collect_syntax(production)?,
            alias: production.copy(alias)?,
            column_aliases: production.copy(column_aliases)?,
            ordinality: *ordinality,
        },
        FromClause::Subquery {
            lateral,
            body,
            alias,
            column_aliases,
        } => FromClause::Subquery {
            lateral: *lateral,
            body: production.boxed(bind_select_with_scope(
                body, resolver, inherited, context, production,
            )?)?,
            alias: production.copy(alias)?,
            column_aliases: production.copy(column_aliases)?,
        },
    })
}

fn bind_join_from(
    from: &FromClause,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &crate::plan::copy::SyntaxProduction<'_>,
) -> Result<FromClause, SQLError> {
    let FromClause::Join {
        comma,
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
    let bound_left = bind_from(left, resolver, inherited, context, production)?;
    let mut right_scope = inherited.copy(&context.control())?;
    if *lateral {
        collect_visible_scope(left, context, &mut right_scope)?;
    }
    let bound_right = bind_from(right, resolver, &right_scope, context, production)?;
    let mut on_scope = inherited.copy(&context.control())?;
    collect_visible_scope(left, context, &mut on_scope)?;
    collect_visible_scope(right, context, &mut on_scope)?;
    Ok(FromClause::Join {
        comma: *comma,
        left: production.boxed(bound_left)?,
        right: production.boxed(bound_right)?,
        kind: *kind,
        on: bind_optional_expr(on.as_ref(), resolver, &on_scope, context, production)?,
        using: production.copy(using)?,
        natural: *natural,
        alias: production.copy(alias)?,
        column_aliases: production.copy(column_aliases)?,
        lateral: *lateral,
    })
}

pub(super) fn bind_ctes(
    ctes: &[CTE],
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &crate::plan::copy::SyntaxProduction<'_>,
) -> Result<Vec<CTE>, SQLError> {
    let mut visible = context.copy()?;
    let mut bound = production.vector(ctes.len())?;
    for cte in ctes {
        visible.prepare_recursive_cte(cte)?;
        bound.push(CTE {
            name: production.copy(&cte.name)?,
            columns: production.copy(&cte.columns)?,
            recursive: cte.recursive,
            materialization: cte.materialization,
            search: production.copy(&cte.search)?,
            cycle: cte
                .cycle
                .as_ref()
                .map(|cycle| -> Result<crate::ast::CteCycleClause, SQLError> {
                    Ok(crate::ast::CteCycleClause {
                        columns: production.copy(&cycle.columns)?,
                        mark_column: production.copy(&cycle.mark_column)?,
                        mark_value: bind_rule_expr_with_scope(
                            &cycle.mark_value,
                            resolver,
                            inherited,
                            &visible,
                            production,
                        )?,
                        mark_default: bind_rule_expr_with_scope(
                            &cycle.mark_default,
                            resolver,
                            inherited,
                            &visible,
                            production,
                        )?,
                        path_column: production.copy(&cycle.path_column)?,
                    })
                })
                .transpose()?,
            body: match &cte.body {
                crate::ast::CteBody::Query(query) => crate::ast::CteBody::Query(production.boxed(
                    bind_select_with_scope(query, resolver, inherited, &visible, production)?,
                )?),
                // Command CTEs cannot reference a rule's event row. Their own DML row-image aliases belong to RETURNING and must retain their local identities.
                body => production.copy(body)?,
            },
        })?;
        visible.record_cte(cte)?;
    }
    bound.finish()
}

fn bind_set_operation(
    operation: &crate::ast::SetOp,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &crate::plan::copy::SyntaxProduction<'_>,
) -> Result<Box<crate::ast::SetOp>, SQLError> {
    production.boxed(crate::ast::SetOp {
        kind: operation.kind,
        all: operation.all,
        left: operation
            .left
            .as_deref()
            .map(|left| {
                bind_select_with_scope(left, resolver, inherited, context, production)
                    .and_then(|value| production.boxed(value))
            })
            .transpose()?,
        right: bind_select_with_scope(&operation.right, resolver, inherited, context, production)?,
        combined_order_by: bind_orders(
            &operation.combined_order_by,
            resolver,
            inherited,
            context,
            production,
        )?,
        combined_limit: bind_optional_expr(
            operation.combined_limit.as_ref(),
            resolver,
            inherited,
            context,
            production,
        )?,
        combined_with_ties: operation.combined_with_ties,
        combined_offset: bind_optional_expr(
            operation.combined_offset.as_ref(),
            resolver,
            inherited,
            context,
            production,
        )?,
    })
}

fn bind_windows(
    windows: &[crate::ast::WindowDefinition],
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &crate::plan::copy::SyntaxProduction<'_>,
) -> Result<Vec<crate::ast::WindowDefinition>, SQLError> {
    windows
        .iter()
        .map(|window| {
            Ok(crate::ast::WindowDefinition {
                name: production.copy(&window.name)?,
                inherited: window.inherited,
                spec: bind_window_spec(&window.spec, resolver, scope, context, production)?,
            })
        })
        .collect_syntax(production)
}
