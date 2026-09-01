//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    collect_expression_subquery_ids, display_relation, retarget_source_expression,
    schema_public_columns, source_qualifier_matches, AutomaticViewLayer, BTreeSet, CteScope,
    Engine, QueryPlan, RelationalPlan, SQLError, ScalarExpr, SourcePlan,
};

fn offset_expression_subquery_ids(expression: &mut ScalarExpr, offset: usize) {
    if offset == 0 {
        return;
    }
    uqa_planner::rewrite_scalar_expression(expression, &mut |node| match node {
        ScalarExpr::ScalarSubquery(id)
        | ScalarExpr::Exists { subquery: id, .. }
        | ScalarExpr::InSubquery { subquery: id, .. } => *id += offset,
        _ => {}
    });
}

fn rewrite_layer_source_scalar(
    expression: &mut ScalarExpr,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    shadowed_qualifiers: &BTreeSet<String>,
    shadowed_columns: &BTreeSet<String>,
) {
    uqa_planner::rewrite_scalar_expression(expression, &mut |node| {
        let replacement = match node {
            ScalarExpr::Column(column)
                if !shadowed_columns.contains(column)
                    && layer.source_schema.has_unqualified_column(column) =>
            {
                Some(ScalarExpr::QualifiedColumn {
                    qualifier: target_qualifier.to_string(),
                    column: column.clone(),
                })
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if !shadowed_qualifiers.contains(qualifier)
                    && source_qualifier_matches(
                        qualifier,
                        &layer.source_qualifier,
                        &layer.source_name,
                    ) =>
            {
                Some(ScalarExpr::QualifiedColumn {
                    qualifier: target_qualifier.to_string(),
                    column: column.clone(),
                })
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            *node = replacement;
        }
    });
}

fn source_plan_declares_qualifier(source: &SourcePlan, qualifier: &str) -> bool {
    match source {
        SourcePlan::Table {
            qualifier: source_qualifier,
            alias,
            ..
        } => alias.as_deref().unwrap_or(source_qualifier) == qualifier,
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            alias.as_deref() == Some(qualifier)
                || source_plan_declares_qualifier(left, qualifier)
                || source_plan_declares_qualifier(right, qualifier)
        }
        SourcePlan::Values { alias, .. } | SourcePlan::Subquery { alias, .. } => {
            alias.as_deref() == Some(qualifier)
        }
        SourcePlan::Function {
            output_name, alias, ..
        } => alias.as_deref().unwrap_or(output_name) == qualifier,
        SourcePlan::FunctionGroup {
            functions, alias, ..
        } => {
            alias.as_deref().or_else(|| {
                functions
                    .first()
                    .map(|function| function.output_name.as_str())
            }) == Some(qualifier)
        }
    }
}

fn rename_source_plan_qualifier(source: &mut SourcePlan, qualifier: &str, replacement: &str) {
    match source {
        SourcePlan::Table {
            qualifier: source_qualifier,
            alias,
            ..
        } => {
            if alias.as_deref() == Some(qualifier) {
                *alias = Some(replacement.to_string());
            } else if alias.is_none() && source_qualifier == qualifier {
                *source_qualifier = replacement.to_string();
            }
        }
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            rename_source_plan_qualifier(left, qualifier, replacement);
            rename_source_plan_qualifier(right, qualifier, replacement);
            if alias.as_deref() == Some(qualifier) {
                *alias = Some(replacement.to_string());
            }
        }
        SourcePlan::Values { alias, .. } | SourcePlan::Subquery { alias, .. } => {
            if alias.as_deref() == Some(qualifier) {
                *alias = Some(replacement.to_string());
            }
        }
        SourcePlan::Function {
            output_name, alias, ..
        } => {
            if alias.as_deref() == Some(qualifier) || alias.is_none() && output_name == qualifier {
                *alias = Some(replacement.to_string());
            }
        }
        SourcePlan::FunctionGroup {
            functions, alias, ..
        } => {
            let default_matches = alias.is_none()
                && functions
                    .first()
                    .is_some_and(|function| function.output_name == qualifier);
            if alias.as_deref() == Some(qualifier) || default_matches {
                *alias = Some(replacement.to_string());
            }
        }
    }
}

fn rename_qualified_scalar(expression: &mut ScalarExpr, qualifier: &str, replacement: &str) {
    uqa_planner::rewrite_scalar_expression(expression, &mut |node| match node {
        ScalarExpr::QualifiedColumn {
            qualifier: current, ..
        }
        | ScalarExpr::QualifiedStar(current)
            if current == qualifier =>
        {
            *current = replacement.to_string();
        }
        _ => {}
    });
}

fn rename_source_plan_scalar_qualifiers(
    source: &mut SourcePlan,
    qualifier: &str,
    replacement: &str,
) {
    match source {
        SourcePlan::Table { .. } | SourcePlan::Subquery { .. } => {}
        SourcePlan::Join {
            left, right, on, ..
        } => {
            rename_source_plan_scalar_qualifiers(left, qualifier, replacement);
            rename_source_plan_scalar_qualifiers(right, qualifier, replacement);
            if let Some(on) = on {
                rename_qualified_scalar(on, qualifier, replacement);
            }
        }
        SourcePlan::Values { rows, .. } => {
            for expression in rows.iter_mut().flatten() {
                rename_qualified_scalar(expression, qualifier, replacement);
            }
        }
        SourcePlan::Function { args, .. } => {
            for expression in args {
                rename_qualified_scalar(expression, qualifier, replacement);
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for expression in functions
                .iter_mut()
                .flat_map(|function| function.args.iter_mut())
            {
                rename_qualified_scalar(expression, qualifier, replacement);
            }
        }
    }
}

fn rename_source_plan_subqueries(
    source: &mut SourcePlan,
    qualifier: &str,
    replacement: &str,
    inherited: bool,
) {
    match source {
        SourcePlan::Join { left, right, .. } => {
            rename_source_plan_subqueries(left, qualifier, replacement, inherited);
            rename_source_plan_subqueries(right, qualifier, replacement, inherited);
        }
        SourcePlan::Subquery { body, .. } => {
            rename_shadowing_query_qualifier(body, qualifier, replacement, inherited);
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
}

fn rename_shadowing_query_qualifier(
    query: &mut QueryPlan,
    qualifier: &str,
    replacement: &str,
    inherited: bool,
) {
    for cte in &mut query.ctes {
        rename_shadowing_query_qualifier(&mut cte.query, qualifier, replacement, inherited);
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                rename_source_plan_subqueries(source, qualifier, replacement, inherited);
            }
            let declared = block
                .from
                .as_ref()
                .is_some_and(|source| source_plan_declares_qualifier(source, qualifier));
            let active = inherited || declared;
            if declared {
                rename_source_plan_qualifier(
                    block.from.as_mut().expect("view subquery source exists"),
                    qualifier,
                    replacement,
                );
            }
            if active {
                if let Some(source) = &mut block.from {
                    rename_source_plan_scalar_qualifiers(source, qualifier, replacement);
                }
                for projection in &mut block.projections {
                    rename_qualified_scalar(&mut projection.expr, qualifier, replacement);
                }
                for expression in block
                    .r#where
                    .iter_mut()
                    .chain(block.group_by.iter_mut())
                    .chain(block.having.iter_mut())
                    .chain(block.limit.iter_mut())
                    .chain(block.offset.iter_mut())
                    .chain(block.distinct_on.iter_mut())
                {
                    rename_qualified_scalar(expression, qualifier, replacement);
                }
                for set in &mut block.grouping_sets {
                    for expression in set {
                        rename_qualified_scalar(expression, qualifier, replacement);
                    }
                }
                for order in &mut block.order_by {
                    rename_qualified_scalar(&mut order.expr, qualifier, replacement);
                }
            }
            for subquery in &mut block.subqueries {
                rename_shadowing_query_qualifier(subquery, qualifier, replacement, active);
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            rename_shadowing_query_qualifier(left, qualifier, replacement, inherited);
            rename_shadowing_query_qualifier(right, qualifier, replacement, inherited);
            if inherited {
                for order in order_by {
                    rename_qualified_scalar(&mut order.expr, qualifier, replacement);
                }
                for expression in [limit.as_deref_mut(), offset.as_deref_mut()]
                    .into_iter()
                    .flatten()
                {
                    rename_qualified_scalar(expression, qualifier, replacement);
                }
            }
            for subquery in subqueries {
                rename_shadowing_query_qualifier(subquery, qualifier, replacement, inherited);
            }
        }
        RelationalPlan::Values { rows, subqueries } => {
            if inherited {
                for expression in rows.iter_mut().flatten() {
                    rename_qualified_scalar(expression, qualifier, replacement);
                }
            }
            for subquery in subqueries {
                rename_shadowing_query_qualifier(subquery, qualifier, replacement, inherited);
            }
        }
    }
}

struct LayerSubqueryRewriteContext<'a> {
    engine: &'a Engine,
    layer: &'a AutomaticViewLayer,
    target_qualifier: &'a str,
}

fn rewrite_layer_source_plan(
    context: &LayerSubqueryRewriteContext<'_>,
    source: &mut SourcePlan,
    scope: &CteScope,
    shadowed_qualifiers: &BTreeSet<String>,
    shadowed_columns: &BTreeSet<String>,
) -> Result<(), SQLError> {
    let rewrite = |expression: &mut ScalarExpr| {
        rewrite_layer_source_scalar(
            expression,
            context.layer,
            context.target_qualifier,
            shadowed_qualifiers,
            shadowed_columns,
        );
    };
    match source {
        SourcePlan::Table { .. } => {}
        SourcePlan::Join {
            left, right, on, ..
        } => {
            rewrite_layer_source_plan(context, left, scope, shadowed_qualifiers, shadowed_columns)?;
            rewrite_layer_source_plan(
                context,
                right,
                scope,
                shadowed_qualifiers,
                shadowed_columns,
            )?;
            if let Some(on) = on {
                rewrite(on);
            }
        }
        SourcePlan::Values { rows, .. } => {
            for expression in rows.iter_mut().flatten() {
                rewrite(expression);
            }
        }
        SourcePlan::Function { args, .. } => {
            for expression in args {
                rewrite(expression);
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for expression in functions
                .iter_mut()
                .flat_map(|function| function.args.iter_mut())
            {
                rewrite(expression);
            }
        }
        SourcePlan::Subquery { body, .. } => {
            rewrite_layer_source_query(
                context,
                body,
                scope,
                shadowed_qualifiers,
                shadowed_columns,
            )?;
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
fn rewrite_layer_source_query(
    context: &LayerSubqueryRewriteContext<'_>,
    query: &mut QueryPlan,
    scope: &CteScope,
    inherited_qualifier_shadows: &BTreeSet<String>,
    inherited_column_shadows: &BTreeSet<String>,
) -> Result<(), SQLError> {
    let mut query_scope = scope.clone();
    for cte in &query.ctes {
        query_scope.insert_deferred(cte.clone());
    }
    for cte in &mut query.ctes {
        rewrite_layer_source_query(
            context,
            &mut cte.query,
            &query_scope,
            inherited_qualifier_shadows,
            inherited_column_shadows,
        )?;
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            let local_schema = block
                .from
                .as_ref()
                .map(|source| {
                    crate::sql::select::analyze_source_plan_schema(
                        context.engine,
                        source,
                        &[],
                        &query_scope,
                        Some(&context.layer.source_schema),
                    )
                })
                .transpose()?;
            let mut qualifier_shadows = inherited_qualifier_shadows.clone();
            if let Some(schema) = local_schema.as_ref() {
                for qualifier in [
                    context.layer.source_qualifier.as_str(),
                    context.layer.source_name.as_str(),
                    display_relation(&context.layer.source_name).as_str(),
                ] {
                    if schema.has_qualifier(qualifier) {
                        qualifier_shadows.insert(qualifier.to_string());
                    }
                }
            }
            let mut column_shadows = inherited_column_shadows.clone();
            if let Some(schema) = local_schema.as_ref() {
                column_shadows.extend(schema_public_columns(schema));
            }
            if let Some(source) = &mut block.from {
                rewrite_layer_source_plan(
                    context,
                    source,
                    &query_scope,
                    &qualifier_shadows,
                    &column_shadows,
                )?;
            }
            let rewrite = |expression: &mut ScalarExpr| {
                rewrite_layer_source_scalar(
                    expression,
                    context.layer,
                    context.target_qualifier,
                    &qualifier_shadows,
                    &column_shadows,
                );
            };
            for projection in &mut block.projections {
                rewrite(&mut projection.expr);
            }
            if let Some(predicate) = &mut block.r#where {
                rewrite(predicate);
            }
            for expression in &mut block.group_by {
                rewrite(expression);
            }
            for set in &mut block.grouping_sets {
                for expression in set {
                    rewrite(expression);
                }
            }
            if let Some(having) = &mut block.having {
                rewrite(having);
            }
            for order in &mut block.order_by {
                rewrite(&mut order.expr);
            }
            for expression in [block.limit.as_mut(), block.offset.as_mut()]
                .into_iter()
                .flatten()
            {
                rewrite(expression);
            }
            for expression in &mut block.distinct_on {
                rewrite(expression);
            }
            for subquery in &mut block.subqueries {
                rewrite_layer_source_query(
                    context,
                    subquery,
                    &query_scope,
                    &qualifier_shadows,
                    &column_shadows,
                )?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            rewrite_layer_source_query(
                context,
                left,
                &query_scope,
                inherited_qualifier_shadows,
                inherited_column_shadows,
            )?;
            rewrite_layer_source_query(
                context,
                right,
                &query_scope,
                inherited_qualifier_shadows,
                inherited_column_shadows,
            )?;
            for order in order_by {
                rewrite_layer_source_scalar(
                    &mut order.expr,
                    context.layer,
                    context.target_qualifier,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                );
            }
            for expression in [limit.as_deref_mut(), offset.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                rewrite_layer_source_scalar(
                    expression,
                    context.layer,
                    context.target_qualifier,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                );
            }
            for subquery in subqueries {
                rewrite_layer_source_query(
                    context,
                    subquery,
                    &query_scope,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                )?;
            }
        }
        RelationalPlan::Values { rows, subqueries } => {
            for expression in rows.iter_mut().flatten() {
                rewrite_layer_source_scalar(
                    expression,
                    context.layer,
                    context.target_qualifier,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                );
            }
            for subquery in subqueries {
                rewrite_layer_source_query(
                    context,
                    subquery,
                    &query_scope,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                )?;
            }
        }
    }
    Ok(())
}

pub(super) fn embed_layer_expression(
    engine: &Engine,
    expression: &ScalarExpr,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    target_subqueries: &mut Vec<QueryPlan>,
) -> Result<ScalarExpr, SQLError> {
    let mut expression = retarget_source_expression(expression, layer, target_qualifier);
    let ids = collect_expression_subquery_ids(std::iter::once(&expression));
    if ids.is_empty() {
        return Ok(expression);
    }
    if ids
        .iter()
        .next_back()
        .is_some_and(|id| *id >= layer.subqueries.len())
    {
        return Err(SQLError::Internal(format!(
            "view `{}` expression has an out-of-bounds scalar subquery slot",
            layer.canonical_name
        )));
    }
    let mut subqueries = layer.subqueries.clone();
    let scope = CteScope::new_for_current_routine(engine);
    let context = LayerSubqueryRewriteContext {
        engine,
        layer,
        target_qualifier,
    };
    for subquery in &mut subqueries {
        rename_shadowing_query_qualifier(
            subquery,
            target_qualifier,
            "\0uqa_view_subquery_local",
            false,
        );
        rewrite_layer_source_query(
            &context,
            subquery,
            &scope,
            &BTreeSet::new(),
            &BTreeSet::new(),
        )?;
    }
    let offset = target_subqueries.len();
    offset_expression_subquery_ids(&mut expression, offset);
    target_subqueries.extend(subqueries);
    Ok(expression)
}
