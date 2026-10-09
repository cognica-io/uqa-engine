//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain automatic-view calculations with physical inputs until their consumer needs them.

use super::AutomaticViewRuleDocument;
use crate::mutation::rules::RuleInputProjection;
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::Value;
use uqa_sql::{
    ast::InternalRelationId, expr::RowLookup, plan::ExpressionPlan,
    semantics::view_rewrite as logical, RowSchema, SQLError, ScalarExpr,
};

pub fn automatic_view_rule_expressions<S: Clone + 'static>(
    request: AutomaticViewRuleDocument<'_, S>,
) -> Result<RuleInputProjection, SQLError> {
    project(
        &request,
        request.view,
        request.required_columns,
        &mut BTreeSet::new(),
    )
}

fn project<S: Clone + 'static>(
    request: &AutomaticViewRuleDocument<'_, S>,
    view: &str,
    required: &BTreeSet<String>,
    visited: &mut BTreeSet<String>,
) -> Result<RuleInputProjection, SQLError> {
    if required.is_empty() {
        return Ok(RuleInputProjection::default());
    }
    let layer = logical::automatic_view_layer(request.context.rewrite, view)?.ok_or_else(|| {
        SQLError::Internal(format!(
            "rewrite-rule view `{view}` is not an automatic view layer"
        ))
    })?;
    if !visited.insert(layer.canonical_name.clone()) {
        return Err(SQLError::Internal(format!(
            "cycle while projecting rewrite-rule row for `{}`",
            layer.canonical_name
        )));
    }
    let selected = layer
        .columns
        .iter()
        .filter(|column| required.contains(&column.name))
        .collect::<Vec<_>>();
    let dependencies = source_dependencies(request, &layer, &selected)?;
    let source = project_source(request, &layer, &dependencies, visited)?;
    let (mut output, mut types, volatile) = bind_projection(request, &layer, &selected, &source)?;
    visited.remove(&layer.canonical_name);
    if volatile {
        for expression in output.values_mut() {
            request
                .context
                .expressions
                .expressions
                .optimize_catalog_scalar(&mut expression.scalar)?;
        }
        let values = selected
            .into_iter()
            .map(|column| {
                let value = crate::query::catalog_expression::eval_stored_expression_plan_with_row(
                    request.context.expressions.expressions,
                    request.scope.clone(),
                    &output[&column.name],
                    &source.source.schema,
                    &source.source.row,
                    request.params,
                )?;
                Ok((
                    column.name.clone(),
                    value,
                    types.remove(&column.name).flatten(),
                ))
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        return Ok(RuleInputProjection::values(values));
    }
    Ok(RuleInputProjection {
        expressions: output,
        source: source.source,
    })
}

type BoundProjection = (
    BTreeMap<String, ExpressionPlan>,
    BTreeMap<String, Option<uqa_sql::ast::ColumnType>>,
    bool,
);

fn bind_projection<S: Clone + 'static>(
    request: &AutomaticViewRuleDocument<'_, S>,
    layer: &logical::AutomaticViewLayer,
    selected: &[&logical::ViewColumn],
    source: &RuleInputProjection,
) -> Result<BoundProjection, SQLError> {
    let relation = InternalRelationId::allocate();
    let aliases = layer
        .source_schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, _)| {
            (
                relation.column(position),
                layer
                    .source_schema
                    .physical_slot(position)
                    .expect("source position"),
                layer.source_schema.column_types()[position].clone(),
            )
        })
        .collect::<Vec<_>>();
    let schema = RowSchema::with_physical_internal_aliases(&layer.source_schema, &aliases);
    let inputs = layer
        .source_schema
        .columns()
        .iter()
        .enumerate()
        .filter_map(|(position, name)| {
            source
                .expressions
                .get(layer.physical_source_column(name))
                .cloned()
                .map(|expression| (relation.column(position), expression))
        })
        .collect();
    let scope = request.context.rewrite.catalog.binding_scope()?;
    let binding = scope.context();
    let mut output = BTreeMap::new();
    let mut types = BTreeMap::new();
    let mut volatile = false;
    for column in selected {
        let mut expression = ExpressionPlan {
            scalar: column.expression.clone(),
            subqueries: layer.subqueries.clone(),
        };
        uqa_sql::plan::subqueries::prune_expression(&mut expression);
        let ty = uqa_sql::binding::bind_expression_plan_routines_for_storage(
            request.context.rewrite.routines,
            &mut expression,
            request.params,
            &binding,
            &schema,
        )?;
        uqa_sql::plan::input_projection::substitute_expression_inputs(&mut expression, &inputs)?;
        volatile |= uqa_sql::semantics::volatility::expression_plan_contains_volatile_function(
            request.context.volatility,
            &expression,
        )?;
        output.insert(column.name.clone(), expression);
        types.insert(column.name.clone(), ty);
    }
    Ok((output, types, volatile))
}

fn project_source<S: Clone + 'static>(
    request: &AutomaticViewRuleDocument<'_, S>,
    layer: &logical::AutomaticViewLayer,
    dependencies: &BTreeSet<String>,
    visited: &mut BTreeSet<String>,
) -> Result<RuleInputProjection, SQLError> {
    let row = if request.document_relation == Some(layer.source_name.as_str()) {
        super::stored_view_document_row(
            request.context.rows,
            &layer.source_name,
            &layer.source_qualifier,
            request.document,
        )?
    } else if request
        .context
        .rewrite
        .catalog
        .view_definition(&layer.source_name)?
        .is_some()
    {
        return project(request, &layer.source_name, dependencies, visited);
    } else {
        crate::mutation::rows::target_row_for_storage_optional(
            request.context.rows,
            &layer.source_name,
            request.storage_table,
            &layer.source_qualifier,
            request.doc_id,
            request.document,
            Some(dependencies),
        )?
    };
    Ok(RuleInputProjection::values(
        layer
            .source_schema
            .columns()
            .iter()
            .enumerate()
            .map(|(position, name)| {
                let name = layer.physical_source_column(name);
                (
                    name.to_string(),
                    row.view().column(name).cloned().unwrap_or(Value::Null),
                    layer.source_schema.column_types()[position].clone(),
                )
            }),
    ))
}

fn source_dependencies<S: Clone + 'static>(
    request: &AutomaticViewRuleDocument<'_, S>,
    layer: &logical::AutomaticViewLayer,
    columns: &[&logical::ViewColumn],
) -> Result<BTreeSet<String>, SQLError> {
    let mut dependencies = BTreeSet::new();
    for column in columns {
        let mut subqueries = BTreeSet::new();
        uqa_sql::semantics::collect_subquery_ids(&column.expression, &mut subqueries);
        if !subqueries.is_empty() {
            dependencies.extend(logical::relation_columns(
                request.context.rewrite,
                &layer.source_name,
            )?);
        }
        column.expression.visit(&mut |node| match node {
            ScalarExpr::Column(name) => {
                dependencies.insert(layer.physical_source_column(name).to_string());
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if qualifier.eq_ignore_ascii_case(&layer.source_qualifier) =>
            {
                dependencies.insert(layer.physical_source_column(column).to_string());
            }
            _ => {}
        });
    }
    Ok(dependencies)
}
