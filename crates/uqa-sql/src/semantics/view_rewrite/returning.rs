//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Automatic-view expression retargeting, check propagation, and RETURNING rewriting.

use super::{
    embed_layer_expression, layer_column, relation_columns, source_qualifier_matches,
    AutomaticViewLayer, ExpressionScope, ProjectionPlan, QueryPlan, ReturningAliases, RowSchema,
    SQLError, ScalarExpr, TriggerEvent, TriggerTiming, ViewCheckOption, ViewCheckPlan,
    ViewRewriteContext, ViewRuleReturningPlan,
};
use crate::semantics::DOC_ID_COLUMN;

pub(super) fn retarget_source_expression(
    expression: &ScalarExpr,
    layer: &AutomaticViewLayer,
    desired_qualifier: &str,
) -> ScalarExpr {
    let mut expression = expression.clone();
    crate::plan::rewrite_scalar_expression(&mut expression, &mut |node| {
        let replacement = match node {
            ScalarExpr::Column(column) => Some(ScalarExpr::QualifiedColumn {
                qualifier: desired_qualifier.to_string(),
                column: layer.physical_source_column(column).to_string(),
            }),
            ScalarExpr::QualifiedColumn { qualifier, column }
                if source_qualifier_matches(
                    qualifier,
                    &layer.source_qualifier,
                    &layer.source_name,
                ) =>
            {
                Some(ScalarExpr::QualifiedColumn {
                    qualifier: desired_qualifier.to_string(),
                    column: layer.physical_source_column(column).to_string(),
                })
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            *node = replacement;
        }
    });
    expression
}

pub(super) fn rewrite_target_expression(
    services: ViewRewriteContext<'_>,
    expression: &mut ScalarExpr,
    layer: &AutomaticViewLayer,
    scope: ExpressionScope<'_>,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<(), SQLError> {
    let mut error = None;
    crate::plan::rewrite_scalar_expression(expression, &mut |node| {
        if error.is_some() {
            return;
        }
        let replacement = match node {
            ScalarExpr::Column(column) => layer_column(layer, column)
                .map(|mapping| {
                    embed_layer_expression(
                        services,
                        &mapping.expression,
                        layer,
                        scope.target_qualifier,
                        subqueries,
                    )
                })
                .or_else(|| {
                    let source = scope.source?;
                    let position = source.unqualified_position(column)?;
                    let identity = source.identity(position)?;
                    identity.qualifier().map(|qualifier| {
                        Ok(ScalarExpr::QualifiedColumn {
                            qualifier: qualifier.to_string(),
                            column: column.clone(),
                        })
                    })
                }),
            ScalarExpr::QualifiedColumn { qualifier, column }
                if scope.target_qualifier(qualifier) =>
            {
                layer_column(layer, column).map(|mapping| {
                    embed_layer_expression(
                        services,
                        &mapping.expression,
                        layer,
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

fn top_level_view_column(
    expression: &ScalarExpr,
    layer: &AutomaticViewLayer,
    scope: ExpressionScope<'_>,
) -> Option<String> {
    match expression {
        ScalarExpr::Column(column) if layer_column(layer, column).is_some() => Some(column.clone()),
        ScalarExpr::QualifiedColumn { qualifier, column }
            if scope.target_qualifier(qualifier) && layer_column(layer, column).is_some() =>
        {
            Some(column.clone())
        }
        _ => None,
    }
}

pub(super) fn rewrite_returning(
    services: ViewRewriteContext<'_>,
    returning: Vec<ProjectionPlan>,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    returning_aliases: &ReturningAliases,
    source: Option<&RowSchema>,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<(Vec<ProjectionPlan>, Vec<usize>), SQLError> {
    let scope = ExpressionScope {
        target_qualifier,
        returning_aliases: Some(returning_aliases),
        source,
        include_excluded: false,
    };
    let mut rewritten = Vec::new();
    let mut source_star_boundaries = Vec::new();
    for projection in returning {
        let bare_star = matches!(projection.expr, ScalarExpr::Star);
        let star_qualifier = match &projection.expr {
            ScalarExpr::Star => Some(target_qualifier),
            ScalarExpr::QualifiedStar(qualifier) if scope.target_qualifier(qualifier) => {
                Some(qualifier.as_str())
            }
            _ => None,
        };
        if let Some(qualifier) = star_qualifier {
            for column in &layer.columns {
                rewritten.push(ProjectionPlan {
                    expr: embed_layer_expression(
                        services,
                        &column.expression,
                        layer,
                        qualifier,
                        subqueries,
                    )?,
                    alias: Some(column.name.clone()),
                });
            }
            if bare_star && source.is_some() {
                source_star_boundaries.push(rewritten.len());
            }
            continue;
        }
        let derived_alias = projection
            .alias
            .clone()
            .or_else(|| top_level_view_column(&projection.expr, layer, scope));
        let mut expression = projection.expr;
        rewrite_target_expression(services, &mut expression, layer, scope, subqueries)?;
        rewritten.push(ProjectionPlan {
            expr: expression,
            alias: derived_alias,
        });
    }
    Ok((rewritten, source_star_boundaries))
}

pub(super) fn rewrite_merge_returning(
    services: ViewRewriteContext<'_>,
    returning: Vec<ProjectionPlan>,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    returning_aliases: &ReturningAliases,
    source: &RowSchema,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<(Vec<ProjectionPlan>, Vec<usize>), SQLError> {
    let scope = ExpressionScope {
        target_qualifier,
        returning_aliases: Some(returning_aliases),
        source: Some(source),
        include_excluded: false,
    };
    let mut rewritten = Vec::new();
    let mut source_star_boundaries = Vec::new();
    for projection in returning {
        let bare_star = matches!(projection.expr, ScalarExpr::Star);
        let star_qualifier = match &projection.expr {
            ScalarExpr::Star => Some(target_qualifier),
            ScalarExpr::QualifiedStar(qualifier) if scope.target_qualifier(qualifier) => {
                Some(qualifier.as_str())
            }
            _ => None,
        };
        if let Some(qualifier) = star_qualifier {
            if bare_star {
                source_star_boundaries.push(rewritten.len());
            }
            for column in &layer.columns {
                rewritten.push(ProjectionPlan {
                    expr: embed_layer_expression(
                        services,
                        &column.expression,
                        layer,
                        qualifier,
                        subqueries,
                    )?,
                    alias: Some(column.name.clone()),
                });
            }
            continue;
        }
        let derived_alias = projection
            .alias
            .clone()
            .or_else(|| top_level_view_column(&projection.expr, layer, scope));
        let mut expression = projection.expr;
        rewrite_target_expression(services, &mut expression, layer, scope, subqueries)?;
        rewritten.push(ProjectionPlan {
            expr: expression,
            alias: derived_alias,
        });
    }
    Ok((rewritten, source_star_boundaries))
}

fn source_star_projections(source: &RowSchema, target_width: usize) -> Vec<ProjectionPlan> {
    source
        .columns()
        .iter()
        .enumerate()
        .filter(|(position, _)| source.wildcard_position_visible(*position))
        .map(|(position, column)| ProjectionPlan {
            expr: ScalarExpr::Position(target_width + position),
            alias: Some(source.public_name(position).unwrap_or(column).to_string()),
        })
        .collect()
}

fn expand_qualified_source_stars(
    returning: Vec<ProjectionPlan>,
    source: &RowSchema,
    target_width: usize,
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let mut expanded = Vec::new();
    for projection in returning {
        let ScalarExpr::QualifiedStar(qualifier) = &projection.expr else {
            expanded.push(projection);
            continue;
        };
        let layout = source.qualified_star_position_layout(qualifier);
        if layout.is_empty() {
            return Err(SQLError::UnknownTable(qualifier.clone()));
        }
        for (column, logical, _, _) in layout {
            if logical.is_some_and(|position| !source.wildcard_position_visible(position)) {
                continue;
            }
            expanded.push(ProjectionPlan {
                expr: logical.map_or_else(
                    || ScalarExpr::QualifiedColumn {
                        qualifier: qualifier.clone(),
                        column: column.clone(),
                    },
                    |position| ScalarExpr::Position(target_width + position),
                ),
                alias: Some(column),
            });
        }
    }
    Ok(expanded)
}

pub(super) fn dml_target_width(
    services: ViewRewriteContext<'_>,
    table: &str,
) -> Result<usize, SQLError> {
    let target_columns = relation_columns(services, table)?;
    Ok(target_columns.len()
        + usize::from(!target_columns.iter().any(|column| column == DOC_ID_COLUMN))
        + 2)
}

pub(super) fn bind_unqualified_source_positions(
    expression: &mut ScalarExpr,
    source: &RowSchema,
    target_width: usize,
) {
    crate::plan::rewrite_scalar_expression(expression, &mut |node| {
        let ScalarExpr::Column(column) = node else {
            return;
        };
        let Some(position) = source.unqualified_position(column) else {
            return;
        };
        if source
            .identity(position)
            .is_some_and(|identity| identity.qualifier().is_none())
        {
            *node = ScalarExpr::Position(target_width + position);
        }
    });
}

pub(super) fn finalize_source_returning(
    services: ViewRewriteContext<'_>,
    table: &str,
    returning: Vec<ProjectionPlan>,
    source: Option<&RowSchema>,
    source_star_boundaries: &[usize],
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let Some(source) = source else {
        return Ok(returning);
    };
    let target_width = dml_target_width(services, table)?;
    let source_star = source_star_projections(source, target_width);
    let mut returning = returning;
    let mut inserted = 0;
    for boundary in source_star_boundaries {
        let position = boundary + inserted;
        returning.splice(position..position, source_star.iter().cloned());
        inserted += source_star.len();
    }
    expand_qualified_source_stars(returning, source, target_width)
}

pub(super) fn rewrite_existing_view_checks(
    services: ViewRewriteContext<'_>,
    checks: &mut [ViewCheckPlan],
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<(), SQLError> {
    let scope = ExpressionScope {
        target_qualifier,
        returning_aliases: None,
        source: None,
        include_excluded: false,
    };
    for check in checks {
        rewrite_target_expression(services, &mut check.predicate, layer, scope, subqueries)?;
    }
    Ok(())
}

pub(super) fn add_check_option(
    services: ViewRewriteContext<'_>,
    checks: &mut Vec<ViewCheckPlan>,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    cascaded: &mut bool,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<(), SQLError> {
    let check_current = *cascaded || layer.check_option != ViewCheckOption::None;
    *cascaded |= layer.check_option == ViewCheckOption::Cascaded;
    if check_current {
        if let Some(predicate) = &layer.predicate {
            checks.insert(
                0,
                ViewCheckPlan {
                    view: layer.canonical_name.clone(),
                    predicate: embed_layer_expression(
                        services,
                        predicate,
                        layer,
                        target_qualifier,
                        subqueries,
                    )?,
                },
            );
        }
    }
    Ok(())
}

pub(super) fn combine_view_predicate(
    services: ViewRewriteContext<'_>,
    current: Option<ScalarExpr>,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    subqueries: &mut Vec<QueryPlan>,
) -> Result<Option<ScalarExpr>, SQLError> {
    let view = layer
        .predicate
        .as_ref()
        .map(|predicate| {
            embed_layer_expression(services, predicate, layer, target_qualifier, subqueries)
        })
        .transpose()?;
    Ok(match (view, current) {
        (Some(view), Some(current)) => Some(ScalarExpr::And(vec![view, current])),
        (Some(view), None) => Some(view),
        (None, current) => current,
    })
}

pub(super) fn instead_of_trigger_definition(
    services: ViewRewriteContext<'_>,
    view: &str,
    event: TriggerEvent,
) -> Result<bool, SQLError> {
    services
        .catalog
        .has_trigger_definition(view, TriggerTiming::InsteadOf, event, true)
}

pub(super) fn record_view_rule_relation(
    services: ViewRewriteContext<'_>,
    relations: &mut Vec<String>,
    layer: &AutomaticViewLayer,
    event: crate::ast::RuleEvent,
) -> Result<bool, SQLError> {
    let has_rules = !services
        .catalog
        .rules_for(&layer.canonical_name, event)?
        .is_empty();
    if has_rules && !relations.contains(&layer.canonical_name) {
        relations.push(layer.canonical_name.clone());
    }
    Ok(has_rules)
}

pub(super) fn preserve_view_rule_returning(
    target: &mut Option<ViewRuleReturningPlan>,
    relation: &str,
    target_qualifier: &str,
    returning: &[ProjectionPlan],
    aliases: &ReturningAliases,
    subqueries: &[QueryPlan],
) {
    if target.is_none() {
        *target = Some(ViewRuleReturningPlan {
            relation: relation.to_string(),
            target_qualifier: target_qualifier.to_string(),
            returning: returning.to_vec(),
            aliases: aliases.clone(),
            subqueries: subqueries.to_vec(),
        });
    }
}
