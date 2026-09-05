//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Correlated DML subquery analysis, validation, and rewriting.

use super::{
    embed_layer_expression, layer_column, validate_view_expression, AutomaticViewLayer, BTreeSet,
    ColumnIdentity, ConflictActionPlan, ConflictPlan, CteScope, DeletePlan, Engine,
    ExpressionScope, InsertPlan, MergePlan, MergeWhenPlan, ProjectionPlan, QueryPlan,
    RelationalPlan, ReturningAliases, RowSchema, SQLError, ScalarExpr, SourcePlan, UpdatePlan,
};

pub(super) fn dml_analysis_scope(
    engine: &Engine,
    ctes: &[uqa_planner::CtePlan],
    subqueries: &[QueryPlan],
) -> CteScope {
    let mut scope = CteScope::new_for_current_routine(engine);
    for cte in ctes {
        scope.insert_deferred(cte.clone());
    }
    scope.scalar_subqueries = subqueries.to_vec();
    scope
}

fn public_view_row_schema(
    engine: &Engine,
    view: &str,
    target_qualifier: &str,
) -> Result<RowSchema, SQLError> {
    let definition = engine
        .view_definition(view)?
        .ok_or_else(|| SQLError::UnknownTable(view.to_string()))?;
    let schema = engine.stored_view_schema(&definition)?;
    let names = schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| schema.public_name(position).unwrap_or(column).to_string())
        .collect();
    Ok(RowSchema::with_qualified_types(
        target_qualifier,
        names,
        schema.column_types().to_vec(),
    ))
}

pub(super) fn collect_expression_subquery_ids<'a>(
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
) -> BTreeSet<usize> {
    let mut ids = BTreeSet::new();
    for expression in expressions {
        crate::sql::select::collect_subquery_ids(expression, &mut ids);
    }
    ids
}

fn dml_correlated_outer_schema(
    engine: &Engine,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    source: Option<&RowSchema>,
    returning_aliases: Option<&ReturningAliases>,
    include_excluded: bool,
) -> Result<(RowSchema, BTreeSet<String>), SQLError> {
    let target = public_view_row_schema(engine, &layer.canonical_name, target_qualifier)?;
    let columns = target.columns().to_vec();
    let types = target.column_types().to_vec();
    let mut outer = target;
    let mut target_qualifiers = BTreeSet::from([target_qualifier.to_string()]);
    if include_excluded {
        let excluded = RowSchema::with_qualified_types("excluded", columns.clone(), types.clone());
        outer = RowSchema::join(&outer, &excluded, std::iter::empty());
        target_qualifiers.insert("excluded".into());
    }
    if let Some(aliases) = returning_aliases {
        let expression_scope = ExpressionScope {
            target_qualifier,
            returning_aliases: Some(aliases),
            source,
            include_excluded: false,
        };
        let mut identities = Vec::new();
        for alias in [&aliases.old, &aliases.new] {
            if !expression_scope.row_image_qualifier(alias) {
                continue;
            }
            target_qualifiers.insert(alias.clone());
            identities.extend(columns.iter().enumerate().map(|(position, column)| {
                (
                    ColumnIdentity::qualified(alias, column),
                    types[position].clone(),
                )
            }));
        }
        outer = RowSchema::with_typed_virtual_identities(&outer, &identities);
    }
    if let Some(source) = source {
        outer = RowSchema::join(&outer, source, std::iter::empty());
    }
    Ok((outer, target_qualifiers))
}

fn validate_correlated_subquery_ids(
    engine: &Engine,
    ctes: &[uqa_planner::CtePlan],
    subqueries: &[QueryPlan],
    ids: &BTreeSet<usize>,
    outer: &RowSchema,
    params: &[uqa_sql::SQLParam],
) -> Result<(), SQLError> {
    let scope = dml_analysis_scope(engine, ctes, subqueries);
    for id in ids {
        let query = subqueries.get(*id).ok_or_else(|| {
            SQLError::Internal(format!("DML scalar subquery slot {id} is out of bounds"))
        })?;
        crate::sql::select::analyze_query_plan_schema(engine, query, params, &scope, Some(outer))?;
    }
    Ok(())
}

fn rewrite_correlated_scalar(
    context: &CorrelatedRewriteContext<'_>,
    expression: &mut ScalarExpr,
    shadowed_qualifiers: &BTreeSet<String>,
    shadowed_columns: &BTreeSet<String>,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<(), SQLError> {
    let mut error = None;
    uqa_planner::rewrite_scalar_expression(expression, &mut |node| {
        if error.is_some() {
            return;
        }
        let replacement = match node {
            ScalarExpr::Column(column) if !shadowed_columns.contains(column) => {
                layer_column(context.layer, column)
                    .map(|mapping| {
                        embed_layer_expression(
                            context.engine,
                            &mapping.expression,
                            context.layer,
                            context.default_target_qualifier,
                            subqueries,
                        )
                    })
                    .or_else(|| {
                        let source = context.source?;
                        let position = source.unqualified_position(column)?;
                        let qualifier = source.identity(position)?.qualifier()?;
                        Some(Ok(ScalarExpr::QualifiedColumn {
                            qualifier: qualifier.to_string(),
                            column: column.clone(),
                        }))
                    })
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if context.target_qualifiers.contains(qualifier)
                    && !shadowed_qualifiers.contains(qualifier) =>
            {
                layer_column(context.layer, column).map(|mapping| {
                    embed_layer_expression(
                        context.engine,
                        &mapping.expression,
                        context.layer,
                        qualifier,
                        subqueries,
                    )
                })
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            match replacement {
                Ok(replacement) => *node = replacement,
                Err(rewrite_error) => error = Some(rewrite_error),
            }
        }
    });
    error.map_or(Ok(()), Err)
}

pub(super) fn schema_public_columns(schema: &RowSchema) -> BTreeSet<String> {
    schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| schema.public_name(position).unwrap_or(column).to_string())
        .collect()
}

struct CorrelatedRewriteContext<'a> {
    engine: &'a Engine,
    layer: &'a AutomaticViewLayer,
    default_target_qualifier: &'a str,
    target_qualifiers: &'a BTreeSet<String>,
    source: Option<&'a RowSchema>,
    params: &'a [uqa_sql::SQLParam],
    outer: &'a RowSchema,
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves scope and subquery identity"
)]
fn rewrite_correlated_query(
    context: &CorrelatedRewriteContext<'_>,
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
        rewrite_correlated_query(
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
                        context.params,
                        &query_scope,
                        Some(context.outer),
                    )
                })
                .transpose()?;
            let mut qualifier_shadows = inherited_qualifier_shadows.clone();
            if let Some(schema) = local_schema.as_ref() {
                qualifier_shadows.extend(
                    context
                        .target_qualifiers
                        .iter()
                        .filter(|qualifier| schema.has_qualifier(qualifier))
                        .cloned(),
                );
            }
            let mut column_shadows = inherited_column_shadows.clone();
            if let Some(schema) = local_schema.as_ref() {
                column_shadows.extend(schema_public_columns(schema));
            }
            let original_subquery_count = block.subqueries.len();
            for subquery in &mut block.subqueries[..original_subquery_count] {
                rewrite_correlated_query(
                    context,
                    subquery,
                    &query_scope,
                    &qualifier_shadows,
                    &column_shadows,
                )?;
            }
            let mut rewrite = |expression: &mut ScalarExpr| {
                rewrite_correlated_scalar(
                    context,
                    expression,
                    &qualifier_shadows,
                    &column_shadows,
                    &mut block.subqueries,
                )
            };
            for projection in &mut block.projections {
                rewrite(&mut projection.expr)?;
            }
            if let Some(predicate) = &mut block.r#where {
                rewrite(predicate)?;
            }
            for expression in &mut block.group_by {
                rewrite(expression)?;
            }
            for set in &mut block.grouping_sets {
                for expression in set {
                    rewrite(expression)?;
                }
            }
            if let Some(having) = &mut block.having {
                rewrite(having)?;
            }
            for order in &mut block.order_by {
                rewrite(&mut order.expr)?;
            }
            if let Some(limit) = &mut block.limit {
                rewrite(limit)?;
            }
            if let Some(offset) = &mut block.offset {
                rewrite(offset)?;
            }
            for expression in &mut block.distinct_on {
                rewrite(expression)?;
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
            rewrite_correlated_query(
                context,
                left,
                &query_scope,
                inherited_qualifier_shadows,
                inherited_column_shadows,
            )?;
            rewrite_correlated_query(
                context,
                right,
                &query_scope,
                inherited_qualifier_shadows,
                inherited_column_shadows,
            )?;
            let original_subquery_count = subqueries.len();
            for subquery in &mut subqueries[..original_subquery_count] {
                rewrite_correlated_query(
                    context,
                    subquery,
                    &query_scope,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                )?;
            }
            for order in order_by {
                rewrite_correlated_scalar(
                    context,
                    &mut order.expr,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                    subqueries,
                )?;
            }
            for expression in [limit.as_deref_mut(), offset.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                rewrite_correlated_scalar(
                    context,
                    expression,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                    subqueries,
                )?;
            }
        }
        RelationalPlan::Values { rows, subqueries } => {
            let original_subquery_count = subqueries.len();
            for subquery in &mut subqueries[..original_subquery_count] {
                rewrite_correlated_query(
                    context,
                    subquery,
                    &query_scope,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                )?;
            }
            for expression in rows.iter_mut().flatten() {
                rewrite_correlated_scalar(
                    context,
                    expression,
                    inherited_qualifier_shadows,
                    inherited_column_shadows,
                    subqueries,
                )?;
            }
        }
    }
    Ok(())
}

pub(super) fn dml_source_schema(
    engine: &Engine,
    source: Option<&SourcePlan>,
    ctes: &[uqa_planner::CtePlan],
    subqueries: &[QueryPlan],
    params: &[uqa_sql::SQLParam],
) -> Result<Option<RowSchema>, SQLError> {
    let Some(source) = source else {
        return Ok(None);
    };
    let scope = dml_analysis_scope(engine, ctes, subqueries);
    crate::sql::select::analyze_source_plan_schema(engine, source, params, &scope, None).map(Some)
}

pub(super) fn insert_input_width(
    engine: &Engine,
    plan: &InsertPlan,
    params: &[uqa_sql::SQLParam],
) -> Result<usize, SQLError> {
    let Some(source) = plan.source.as_deref() else {
        return Ok(plan.rows.first().map_or(0, Vec::len));
    };
    let scope = dml_analysis_scope(engine, &plan.ctes, &plan.subqueries);
    Ok(crate::sql::select::analyze_query_plan_schema(engine, source, params, &scope, None)?.len())
}

pub(super) fn returning_subquery_ids(returning: &[ProjectionPlan]) -> BTreeSet<usize> {
    collect_expression_subquery_ids(returning.iter().map(|projection| &projection.expr))
}

pub(super) fn merge_matched_subquery_ids(plan: &MergePlan) -> BTreeSet<usize> {
    let mut ids = collect_expression_subquery_ids(std::iter::once(&plan.join_condition));
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => {
                ids.extend(collect_expression_subquery_ids(condition.iter()));
                ids.extend(collect_expression_subquery_ids(
                    assignments.iter().map(|assignment| &assignment.value),
                ));
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition } => {
                ids.extend(collect_expression_subquery_ids(condition.iter()));
            }
            _ => {}
        }
    }
    ids
}

pub(super) fn merge_target_only_subquery_ids(plan: &MergePlan) -> BTreeSet<usize> {
    let mut ids = BTreeSet::new();
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                ids.extend(collect_expression_subquery_ids(condition.iter()));
                ids.extend(collect_expression_subquery_ids(
                    assignments.iter().map(|assignment| &assignment.value),
                ));
            }
            MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                ids.extend(collect_expression_subquery_ids(condition.iter()));
            }
            _ => {}
        }
    }
    ids
}

pub(super) fn insert_conflict_subquery_ids(plan: &InsertPlan) -> BTreeSet<usize> {
    let Some(ConflictPlan {
        action:
            ConflictActionPlan::Update {
                assignments,
                predicate,
            },
        ..
    }) = &plan.on_conflict
    else {
        return BTreeSet::new();
    };
    let mut ids =
        collect_expression_subquery_ids(assignments.iter().map(|assignment| &assignment.value));
    ids.extend(collect_expression_subquery_ids(
        predicate.iter().map(Box::as_ref),
    ));
    ids
}

pub(super) fn update_ordinary_subquery_ids(plan: &UpdatePlan) -> BTreeSet<usize> {
    let mut ids = collect_expression_subquery_ids(
        plan.assignments.iter().map(|assignment| &assignment.value),
    );
    ids.extend(collect_expression_subquery_ids(plan.predicate.iter()));
    ids
}

pub(super) fn delete_ordinary_subquery_ids(plan: &DeletePlan) -> BTreeSet<usize> {
    collect_expression_subquery_ids(plan.predicate.iter())
}

pub(super) struct CorrelatedDmlContext<'a> {
    pub(super) engine: &'a Engine,
    pub(super) layer: &'a AutomaticViewLayer,
    pub(super) target_qualifier: &'a str,
    pub(super) source: Option<&'a RowSchema>,
    pub(super) returning_aliases: Option<&'a ReturningAliases>,
    pub(super) include_excluded: bool,
    pub(super) ctes: &'a [uqa_planner::CtePlan],
    pub(super) ids: &'a BTreeSet<usize>,
    pub(super) params: &'a [uqa_sql::SQLParam],
}

fn validate_correlated_dml_context(
    context: CorrelatedDmlContext<'_>,
    subqueries: &[QueryPlan],
) -> Result<(), SQLError> {
    let (outer, _) = dml_correlated_outer_schema(
        context.engine,
        context.layer,
        context.target_qualifier,
        context.source,
        context.returning_aliases,
        context.include_excluded,
    )?;
    validate_correlated_subquery_ids(
        context.engine,
        context.ctes,
        subqueries,
        context.ids,
        &outer,
        context.params,
    )
}

pub(super) fn rewrite_correlated_dml_context(
    context: CorrelatedDmlContext<'_>,
    subqueries: &mut [QueryPlan],
) -> Result<(), SQLError> {
    let (outer, target_qualifiers) = dml_correlated_outer_schema(
        context.engine,
        context.layer,
        context.target_qualifier,
        context.source,
        context.returning_aliases,
        context.include_excluded,
    )?;
    let scope = dml_analysis_scope(context.engine, context.ctes, subqueries);
    let rewrite_context = CorrelatedRewriteContext {
        engine: context.engine,
        layer: context.layer,
        default_target_qualifier: context.target_qualifier,
        target_qualifiers: &target_qualifiers,
        source: context.source,
        params: context.params,
        outer: &outer,
    };
    for id in context.ids {
        let query = subqueries.get_mut(*id).ok_or_else(|| {
            SQLError::Internal(format!("DML scalar subquery slot {id} is out of bounds"))
        })?;
        rewrite_correlated_query(
            &rewrite_context,
            query,
            &scope,
            &BTreeSet::new(),
            &BTreeSet::new(),
        )?;
    }
    Ok(())
}

pub(super) fn validate_update_expressions(
    engine: &Engine,
    plan: &UpdatePlan,
    layer: &AutomaticViewLayer,
    source: Option<&RowSchema>,
    params: &[uqa_sql::SQLParam],
) -> Result<(), SQLError> {
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source,
            returning_aliases: None,
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &update_ordinary_subquery_ids(plan),
            params,
        },
        &plan.subqueries,
    )?;
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source,
            returning_aliases: Some(&plan.returning_aliases),
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &returning_subquery_ids(&plan.returning),
            params,
        },
        &plan.subqueries,
    )?;
    let ordinary_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source,
        include_excluded: false,
    };
    for assignment in &plan.assignments {
        validate_view_expression(&assignment.value, layer, ordinary_scope)?;
    }
    if let Some(predicate) = &plan.predicate {
        validate_view_expression(predicate, layer, ordinary_scope)?;
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..ordinary_scope
    };
    for projection in &plan.returning {
        validate_view_expression(&projection.expr, layer, returning_scope)?;
    }
    Ok(())
}

pub(super) fn validate_delete_expressions(
    engine: &Engine,
    plan: &DeletePlan,
    layer: &AutomaticViewLayer,
    source: Option<&RowSchema>,
    params: &[uqa_sql::SQLParam],
) -> Result<(), SQLError> {
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source,
            returning_aliases: None,
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &delete_ordinary_subquery_ids(plan),
            params,
        },
        &plan.subqueries,
    )?;
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source,
            returning_aliases: Some(&plan.returning_aliases),
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &returning_subquery_ids(&plan.returning),
            params,
        },
        &plan.subqueries,
    )?;
    let ordinary_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source,
        include_excluded: false,
    };
    if let Some(predicate) = &plan.predicate {
        validate_view_expression(predicate, layer, ordinary_scope)?;
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..ordinary_scope
    };
    for projection in &plan.returning {
        validate_view_expression(&projection.expr, layer, returning_scope)?;
    }
    Ok(())
}

pub(super) fn validate_merge_expressions(
    engine: &Engine,
    plan: &MergePlan,
    layer: &AutomaticViewLayer,
    source: &RowSchema,
    params: &[uqa_sql::SQLParam],
) -> Result<(), SQLError> {
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: Some(source),
            returning_aliases: None,
            include_excluded: false,
            ctes: &[],
            ids: &merge_matched_subquery_ids(plan),
            params,
        },
        &plan.subqueries,
    )?;
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: None,
            returning_aliases: None,
            include_excluded: false,
            ctes: &[],
            ids: &merge_target_only_subquery_ids(plan),
            params,
        },
        &plan.subqueries,
    )?;
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: Some(source),
            returning_aliases: Some(&plan.returning_aliases),
            include_excluded: false,
            ctes: &[],
            ids: &returning_subquery_ids(&plan.returning),
            params,
        },
        &plan.subqueries,
    )?;
    let matched_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source: Some(source),
        include_excluded: false,
    };
    let target_only_scope = ExpressionScope {
        source: None,
        ..matched_scope
    };
    validate_view_expression(&plan.join_condition, layer, matched_scope)?;
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => {
                if let Some(condition) = condition {
                    validate_view_expression(condition, layer, matched_scope)?;
                }
                for assignment in assignments {
                    validate_view_expression(&assignment.value, layer, matched_scope)?;
                }
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition } => {
                if let Some(condition) = condition {
                    validate_view_expression(condition, layer, matched_scope)?;
                }
            }
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                if let Some(condition) = condition {
                    validate_view_expression(condition, layer, target_only_scope)?;
                }
                for assignment in assignments {
                    validate_view_expression(&assignment.value, layer, target_only_scope)?;
                }
            }
            MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                if let Some(condition) = condition {
                    validate_view_expression(condition, layer, target_only_scope)?;
                }
            }
            MergeWhenPlan::InsertNotMatched { .. } | MergeWhenPlan::NothingNotMatched { .. } => {}
        }
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..matched_scope
    };
    for projection in &plan.returning {
        validate_view_expression(&projection.expr, layer, returning_scope)?;
    }
    Ok(())
}

pub(super) fn validate_insert_expressions(
    engine: &Engine,
    plan: &InsertPlan,
    layer: &AutomaticViewLayer,
    params: &[uqa_sql::SQLParam],
) -> Result<(), SQLError> {
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: None,
            returning_aliases: None,
            include_excluded: true,
            ctes: &plan.ctes,
            ids: &insert_conflict_subquery_ids(plan),
            params,
        },
        &plan.subqueries,
    )?;
    validate_correlated_dml_context(
        CorrelatedDmlContext {
            engine,
            layer,
            target_qualifier: &plan.target_qualifier,
            source: None,
            returning_aliases: Some(&plan.returning_aliases),
            include_excluded: false,
            ctes: &plan.ctes,
            ids: &returning_subquery_ids(&plan.returning),
            params,
        },
        &plan.subqueries,
    )?;
    if let Some(conflict) = &plan.on_conflict {
        if let ConflictActionPlan::Update {
            assignments,
            predicate,
        } = &conflict.action
        {
            let scope = ExpressionScope {
                target_qualifier: &plan.target_qualifier,
                returning_aliases: None,
                source: None,
                include_excluded: true,
            };
            for assignment in assignments {
                validate_view_expression(&assignment.value, layer, scope)?;
            }
            if let Some(predicate) = predicate {
                validate_view_expression(predicate, layer, scope)?;
            }
        }
    }
    let scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: Some(&plan.returning_aliases),
        source: None,
        include_excluded: false,
    };
    for projection in &plan.returning {
        validate_view_expression(&projection.expr, layer, scope)?;
    }
    Ok(())
}
