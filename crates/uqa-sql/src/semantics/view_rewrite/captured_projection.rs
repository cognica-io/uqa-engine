//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind a view's output expressions to private raw input attributes without executing them.

use super::{automatic_view_layer, AutomaticViewLayer, DeferredOuterRewrite, ViewRewriteContext};
use crate::{
    ast::{InternalColumnRef, InternalRelationId},
    plan::{ExpressionPlan, ProjectionPlan, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan},
    ColumnIdentity, RowSchema, SQLError, ScalarExpr,
};
use std::collections::BTreeMap;
mod owned;
pub use owned::{capture_view_projection_owned, OwnedCapturedViewProjection};

pub struct CapturedViewProjection {
    pub source: SourcePlan,
    pub expressions: BTreeMap<InternalColumnRef, ExpressionPlan>,
}

/// The caller decides whether the selected expressions may cross the producer boundary. The replacement source retains the view predicate, namespace and owner.
pub fn capture_view_projection(
    services: ViewRewriteContext<'_>,
    relation: InternalRelationId,
    query: &QueryPlan,
) -> Result<Option<CapturedViewProjection>, SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(None);
    };
    let Some(SourcePlan::Table {
        name,
        qualifier,
        alias,
        ..
    }) = &block.from
    else {
        return Ok(None);
    };
    if !query.ctes.is_empty() || !plain_projection(block) {
        return Ok(None);
    }
    let Some(layer) = automatic_view_layer(services, name)? else {
        return Ok(None);
    };
    let definition = services
        .catalog
        .view_definition(&layer.canonical_name)?
        .ok_or_else(|| SQLError::UnknownTable(name.clone()))?;
    let qualifier = alias.as_deref().unwrap_or(qualifier);
    let raw = InternalRelationId::allocate();
    let schema = input_schema(&layer.source_schema, raw);
    let scope = services.catalog.binding_scope()?;
    let subject = crate::semantics::view_privileges::next_layer_privilege_subject(
        services.authorization,
        &layer.canonical_name,
        query.relation_privilege_subject.as_ref(),
    )?;
    services.authorization.ensure_query_select(query)?;
    let names = bind_outputs(
        services,
        &layer,
        qualifier,
        &schema,
        &scope.context(),
        &subject,
        raw,
    )?;
    let rewrite = DeferredOuterRewrite::from_inputs(names);
    let expressions = capture_outputs(relation, block, &layer, qualifier, &rewrite);
    let mut producer = definition.query;
    producer.relation_privilege_subject = Some(subject);
    producer.relations_bound = true;
    // The stored view requires its declared source permissions even when consumers omit outputs.
    services.authorization.ensure_query_select(&producer)?;
    let RelationalPlan::QueryBlock(input) = &mut producer.root else {
        unreachable!("automatic view query");
    };
    input.projections = (0..schema.len())
        .map(|position| ProjectionPlan {
            expr: ScalarExpr::Position(position),
            alias: Some(schema.columns()[position].clone()),
        })
        .collect();
    input
        .projections
        .extend(virtual_columns(&schema).map(|identity| ProjectionPlan {
            expr: ScalarExpr::qualified_column(
                identity.qualifier().expect("qualified metadata"),
                identity.column(),
            ),
            alias: Some(identity.column().to_owned()),
        }));
    let raw_names = input
        .projections
        .iter()
        .map(|projection| projection.alias.clone().expect("raw attribute"))
        .collect::<Vec<_>>();
    producer.analyzed_output = None;
    producer.analyzed_source = None;
    crate::plan::subqueries::prune_query(&mut producer);
    let captured = CapturedViewProjection {
        source: SourcePlan::Subquery {
            captured_output: Some(raw),
            lateral: false,
            body: Box::new(producer),
            alias: None,
            column_aliases: Vec::new(),
        },
        expressions,
    };
    Ok(Some(captured.filter(query, raw, &raw_names, &rewrite)?))
}

fn plain_projection(block: &QueryBlockPlan) -> bool {
    matches!(block.compute, crate::plan::ComputePlan::Project)
        && block.deferred_outer_rewrites.is_empty()
        && block.group_by.is_empty()
        && block.grouping_sets.is_empty()
        && block.having.is_none()
        && !block.distinct
        && block.distinct_on.is_empty()
        && block.windows.is_empty()
        && block.order_by.is_empty()
        && block.limit.is_none()
        && block.offset.is_none()
        && block.locking.is_empty()
}

fn bind_outputs(
    services: ViewRewriteContext<'_>,
    layer: &AutomaticViewLayer,
    qualifier: &str,
    schema: &RowSchema,
    scope: &crate::binding::BindingContext<'_>,
    subject: &crate::catalog::roles::RoleReference,
    raw: InternalRelationId,
) -> Result<BTreeMap<ColumnIdentity, ExpressionPlan>, SQLError> {
    let metadata = metadata_rewrite(schema, raw);
    let mut names = BTreeMap::new();
    let mut fields = Vec::new();
    let mut arena = Vec::new();
    for column in &layer.columns {
        let mut expression = ExpressionPlan {
            scalar: column.expression.clone(),
            subqueries: layer.subqueries.clone(),
        };
        crate::plan::subqueries::prune_expression(&mut expression);
        crate::binding::bind_expression_plan_routines_for_storage(
            services.routines,
            &mut expression,
            &[],
            scope,
            schema,
        )?;
        metadata.bind_expression(&mut expression);
        for query in &mut expression.subqueries {
            query.relation_privilege_subject = Some(subject.clone());
        }
        fields.push(crate::plan::input_projection::append_expression(
            &mut arena,
            expression.clone(),
        ));
        names.insert(
            ColumnIdentity::qualified(qualifier, &column.name),
            expression.clone(),
        );
        names.insert(ColumnIdentity::unqualified(&column.name), expression);
    }
    names
        .entry(ColumnIdentity::unqualified(qualifier))
        .or_insert(ExpressionPlan {
            scalar: ScalarExpr::Cast {
                implicit: true,
                expr: Box::new(ScalarExpr::Row(fields)),
                ty: layer.canonical_name.clone(),
            },
            subqueries: arena,
        });
    Ok(names)
}

fn capture_outputs(
    relation: InternalRelationId,
    block: &QueryBlockPlan,
    layer: &AutomaticViewLayer,
    qualifier: &str,
    rewrite: &DeferredOuterRewrite,
) -> BTreeMap<InternalColumnRef, ExpressionPlan> {
    block
        .projections
        .iter()
        .enumerate()
        .map(|(position, projection)| {
            let mut expression = ExpressionPlan {
                scalar: match &projection.expr {
                    ScalarExpr::Position(position) => {
                        ScalarExpr::qualified_column(qualifier, &layer.columns[*position].name)
                    }
                    expression => expression.clone(),
                },
                subqueries: block.subqueries.clone(),
            };
            rewrite.bind_expression(&mut expression);
            (relation.column(position), expression)
        })
        .collect()
}

impl CapturedViewProjection {
    /// Keep the caller's qualification above the view's own predicate, including security-barrier views, while exposing only raw attributes to its later output expressions.
    fn filter(
        mut self,
        query: &QueryPlan,
        raw: InternalRelationId,
        raw_names: &[String],
        rewrite: &DeferredOuterRewrite,
    ) -> Result<Self, SQLError> {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!("captured query block");
        };
        let Some(predicate) = &block.r#where else {
            return Ok(self);
        };
        let mut predicate = ExpressionPlan {
            scalar: predicate.clone(),
            subqueries: block.subqueries.clone(),
        };
        rewrite.bind_expression(&mut predicate);
        crate::plan::subqueries::prune_expression(&mut predicate);
        let filtered = InternalRelationId::allocate();
        let replacements = (0..raw_names.len())
            .map(|position| {
                (
                    raw.column(position),
                    ExpressionPlan {
                        scalar: ScalarExpr::InternalColumn(filtered.column(position)),
                        subqueries: Vec::new(),
                    },
                )
            })
            .collect();
        for expression in self.expressions.values_mut() {
            crate::plan::input_projection::substitute_expression_inputs(expression, &replacements)?;
        }
        let mut filter = query.clone();
        filter.analyzed_output = None;
        filter.analyzed_source = None;
        let RelationalPlan::QueryBlock(block) = &mut filter.root else {
            unreachable!("captured query block");
        };
        block.from = Some(self.source);
        block.r#where = Some(predicate.scalar);
        block.subqueries = predicate.subqueries;
        block.privilege_columns.clear();
        block.projections = raw_names
            .iter()
            .enumerate()
            .map(|(position, name)| ProjectionPlan {
                expr: ScalarExpr::InternalColumn(raw.column(position)),
                alias: Some(name.clone()),
            })
            .collect();
        self.source = SourcePlan::Subquery {
            captured_output: Some(filtered),
            lateral: false,
            body: Box::new(filter),
            alias: None,
            column_aliases: Vec::new(),
        };
        Ok(self)
    }
}

fn virtual_columns(schema: &RowSchema) -> impl Iterator<Item = &ColumnIdentity> {
    schema
        .typed_virtual_identities()
        .map(|(identity, _)| identity)
        .filter(|identity| identity.qualifier().is_some())
}

fn input_schema(source: &RowSchema, raw: InternalRelationId) -> RowSchema {
    let source = crate::binding::with_query_table_pseudo_columns(source);
    let aliases = (0..source.len())
        .map(|position| {
            (
                raw.column(position),
                source.physical_slot(position).expect("view input"),
                source.column_types()[position].clone(),
            )
        })
        .collect::<Vec<_>>();
    RowSchema::with_physical_internal_aliases(&source, &aliases)
}

fn metadata_rewrite(schema: &RowSchema, raw: InternalRelationId) -> DeferredOuterRewrite {
    let mut names = BTreeMap::new();
    for (position, identity) in virtual_columns(schema).enumerate() {
        let expression = ExpressionPlan {
            scalar: ScalarExpr::InternalColumn(raw.column(schema.len() + position)),
            subqueries: Vec::new(),
        };
        names.insert(identity.clone(), expression.clone());
        if schema.unqualified_position(identity.column()).is_none() {
            names
                .entry(ColumnIdentity::unqualified(identity.column()))
                .or_insert(expression);
        }
    }
    DeferredOuterRewrite::from_inputs(names)
}

#[cfg(test)]
mod tests;
