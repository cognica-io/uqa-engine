//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured view outputs retain admitted expressions, source boxes and lexical substitutions.

use super::{plain_projection, virtual_columns, DeferredOuterRewrite, ViewRewriteContext};
use crate::{
    ast::InternalRelationId,
    binding::production::ControlledBindingContext,
    catalog::roles::RoleReference,
    plan::{
        input_projection::OwnedInputProjections, MutableExpressionPlan, MutableQueryPlan,
        MutableSourcePlan, OwnedProjection, OwnedScalarExpr, QueryBlockPlan, QueryPlan,
        RelationalPlan, ScalarQueryArena, SourcePlan,
    },
    production::Buffer,
    semantics::view_rewrite::production::{automatic_view_layer_owned, OwnedAutomaticViewLayer},
    ColumnIdentity, RowSchema, SQLError, ScalarExpr,
};
use uqa_core::memory::{OwnedMap, Produced, ProductionControl, ProductionVec};

type Names = OwnedMap<Produced<ColumnIdentity>, Produced<crate::plan::ExpressionPlan>>;

pub struct OwnedCapturedViewProjection {
    pub source: MutableSourcePlan,
    pub expressions: OwnedInputProjections,
}

pub fn capture_view_projection_owned(
    services: ViewRewriteContext<'_>,
    relation: InternalRelationId,
    query: &QueryPlan,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedCapturedViewProjection>, SQLError> {
    control.check()?;
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
    let Some(layer) = automatic_view_layer_owned(services, name, control)? else {
        return Ok(None);
    };
    let mut definition = None;
    services.catalog.with_view_definition_with_control(
        &layer.canonical_name,
        control,
        &mut |view| {
            let view = view.ok_or_else(|| SQLError::UnknownTable(name.clone()))?;
            definition = Some(view.query.copy_for_rewrite(control)?);
            Ok(())
        },
    )?;
    let definition = definition
        .ok_or_else(|| SQLError::Internal("view catalog did not supply its definition".into()))?;
    let qualifier = alias.as_deref().unwrap_or(qualifier);
    let raw = InternalRelationId::allocate();
    let schema = input_schema(&layer.source_schema, raw, control)?;
    let scope = services.catalog.binding_scope_with_control(control)?;
    let subject = crate::semantics::view_privileges::next_layer_privilege_subject_with_control(
        services.authorization,
        &layer.canonical_name,
        query.relation_privilege_subject.as_ref(),
        control,
    )?;
    services
        .authorization
        .ensure_query_select_with_control(query, control)?;
    let names = bind_outputs(
        services,
        &layer,
        qualifier,
        &schema,
        scope.context_with_control(control)?,
        &subject,
        raw,
        control,
    )?;
    let rewrite = DeferredOuterRewrite::from_inputs_owned(names, control)?;
    let expressions = capture_outputs(relation, block, &layer, qualifier, &rewrite, control)?;
    let mut producer = definition
        .set_privilege_subject(Some(subject), control)?
        .set_relations_bound(true, control)?;
    services
        .authorization
        .ensure_query_select_with_control(&producer, control)?;
    let mut projections = Buffer::new(control);
    let mut raw_names = ProductionVec::new(*control);
    for (position, name) in schema.columns().iter().enumerate() {
        projections.push(
            OwnedProjection {
                expression: ScalarExpr::Position(position).copy_for_rewrite(control)?,
                alias: Some(control.copy_text(name)?),
            },
            control,
        )?;
        raw_names.push_produced(control.copy_text(name)?)?;
    }
    for identity in virtual_columns(&schema) {
        projections.push(
            OwnedProjection {
                expression: OwnedScalarExpr::column(
                    control.copy_text(identity.column())?,
                    Some(control.copy_text(identity.qualifier().expect("qualified metadata"))?),
                    control,
                )?,
                alias: Some(control.copy_text(identity.column())?),
            },
            control,
        )?;
        raw_names.push_produced(control.copy_text(identity.column())?)?;
    }
    producer = producer
        .replace_projections(projections.into_iter(), control)?
        .set_analyzed_output(None, control)?
        .discard_source_analysis(control)?
        .prune_subqueries(control)?;
    let captured = OwnedCapturedViewProjection {
        source: source(producer, raw, control)?,
        expressions,
    };
    Ok(Some(
        captured.filter(query, raw, &raw_names, &rewrite, control)?,
    ))
}

fn input_schema(
    source: &RowSchema,
    raw: InternalRelationId,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let source = crate::binding::with_query_table_pseudo_columns_with_control(source, control)?;
    RowSchema::with_physical_internal_aliases_with_control(
        &source,
        (0..source.len()).map(|position| {
            (
                raw.column(position),
                source.physical_slot(position).expect("view input"),
                source.column_type(position),
            )
        }),
        control,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "one captured view binding retains its namespace, authority and original request"
)]
fn bind_outputs(
    services: ViewRewriteContext<'_>,
    layer: &OwnedAutomaticViewLayer,
    qualifier: &str,
    schema: &RowSchema,
    scope: ControlledBindingContext<'_>,
    subject: &RoleReference,
    raw: InternalRelationId,
    control: &ProductionControl<'_>,
) -> Result<Names, SQLError> {
    let metadata = metadata_rewrite(schema, raw, control)?;
    let mut names = Names::new();
    let mut fields = Buffer::new(control);
    let mut arena = ScalarQueryArena::copy_from(&[], control)?;
    for column in layer.columns.iter() {
        let expression = MutableExpressionPlan::copy_from(
            &column.expression,
            layer.subqueries.queries(),
            control,
        )?
        .prune_subqueries(control)?;
        let expression = crate::binding::bind_expression_plan_routines_for_storage_owned(
            services.routines,
            expression,
            &[],
            scope.copy_with_control(control)?,
            schema,
            control,
        )?;
        let mut expression = metadata.bind_expression_owned(expression, control)?;
        for index in 0..expression.queries().len() {
            expression = expression.map_query(index, control, |query| {
                query.set_privilege_subject(Some(subject.clone_with_control(control)?), control)
            })?;
        }
        let expression = expression.finish(control)?;
        fields.push(
            arena.append_expression(expression.copy_for_rewrite(control)?, control)?,
            control,
        )?;
        insert(
            &mut names,
            ColumnIdentity::from_parts_with_control(Some(qualifier), &column.name, control)?,
            expression.copy_for_rewrite(control)?.finish(control)?,
            control,
        )?;
        insert(
            &mut names,
            ColumnIdentity::from_parts_with_control(None, &column.name, control)?,
            expression,
            control,
        )?;
    }
    let key = ColumnIdentity::from_parts_with_control(None, qualifier, control)?;
    if names.get_by(|identity| identity.cmp(&key)).is_none() {
        let row = OwnedScalarExpr::row(fields.into_iter(), control)?;
        let scalar = OwnedScalarExpr::cast(
            row,
            control.copy_text(&layer.canonical_name)?,
            true,
            control,
        )?;
        let expression =
            MutableExpressionPlan::from_parts(scalar, arena, control)?.finish(control)?;
        insert(&mut names, key, expression, control)?;
    }
    Ok(names)
}

fn capture_outputs(
    relation: InternalRelationId,
    block: &QueryBlockPlan,
    layer: &OwnedAutomaticViewLayer,
    qualifier: &str,
    rewrite: &DeferredOuterRewrite,
    control: &ProductionControl<'_>,
) -> Result<OwnedInputProjections, SQLError> {
    let mut expressions = OwnedInputProjections::new();
    for (position, projection) in block.projections.iter().enumerate() {
        let scalar = match &projection.expr {
            ScalarExpr::Position(position) => OwnedScalarExpr::column(
                control.copy_text(&layer.columns[*position].name)?,
                Some(control.copy_text(qualifier)?),
                control,
            )?,
            expression => expression.copy_for_rewrite(control)?,
        };
        let expression = MutableExpressionPlan::from_parts(
            scalar,
            ScalarQueryArena::copy_from(&block.subqueries, control)?,
            control,
        )?;
        let expression = rewrite
            .bind_expression_owned(expression, control)?
            .finish(control)?;
        insert(
            &mut expressions,
            relation.column(position),
            expression,
            control,
        )?;
    }
    Ok(expressions)
}

fn metadata_rewrite(
    schema: &RowSchema,
    raw: InternalRelationId,
    control: &ProductionControl<'_>,
) -> Result<DeferredOuterRewrite, SQLError> {
    let mut names = Names::new();
    for (position, identity) in virtual_columns(schema).enumerate() {
        let expression = MutableExpressionPlan::copy_from(
            &ScalarExpr::InternalColumn(raw.column(schema.len() + position)),
            &[],
            control,
        )?
        .finish(control)?;
        insert(
            &mut names,
            ColumnIdentity::from_parts_with_control(
                identity.qualifier(),
                identity.column(),
                control,
            )?,
            expression.copy_for_rewrite(control)?.finish(control)?,
            control,
        )?;
        if schema.unqualified_position(identity.column()).is_none() {
            let key = ColumnIdentity::from_parts_with_control(None, identity.column(), control)?;
            if names.get_by(|identity| identity.cmp(&key)).is_none() {
                insert(&mut names, key, expression, control)?;
            }
        }
    }
    DeferredOuterRewrite::from_inputs_owned(names, control)
}

fn source(
    query: MutableQueryPlan,
    relation: InternalRelationId,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    MutableSourcePlan::subquery(
        query,
        None,
        control.finish(Vec::new(), control.empty_reservation())?,
        Some(relation),
        false,
        control,
    )
}

fn insert<K: Ord, V>(
    map: &mut OwnedMap<K, V>,
    key: K,
    value: V,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if let Some(budget) = control.budget() {
        map.insert_budgeted(key, value, budget)?;
    } else {
        map.insert(key, value);
    }
    Ok(())
}

impl OwnedCapturedViewProjection {
    fn filter(
        mut self,
        query: &QueryPlan,
        raw: InternalRelationId,
        raw_names: &[String],
        rewrite: &DeferredOuterRewrite,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!("captured query block")
        };
        let Some(predicate) = &block.r#where else {
            return Ok(self);
        };
        let predicate = MutableExpressionPlan::copy_from(predicate, &block.subqueries, control)?;
        let predicate = rewrite
            .bind_expression_owned(predicate, control)?
            .prune_subqueries(control)?;
        let filtered = InternalRelationId::allocate();
        let mut replacements = OwnedInputProjections::new();
        for position in 0..raw_names.len() {
            let expression = MutableExpressionPlan::copy_from(
                &ScalarExpr::InternalColumn(filtered.column(position)),
                &[],
                control,
            )?
            .finish(control)?;
            insert(&mut replacements, raw.column(position), expression, control)?;
        }
        let mut expressions = OwnedInputProjections::new();
        for (column, expression) in self.expressions {
            let expression = crate::plan::input_projection::substitute_expression_inputs_owned(
                expression.copy_for_rewrite(control)?,
                &replacements,
                control,
            )?
            .finish(control)?;
            insert(&mut expressions, column, expression, control)?;
        }
        self.expressions = expressions;
        let mut projections = Buffer::new(control);
        for (position, name) in raw_names.iter().enumerate() {
            projections.push(
                OwnedProjection {
                    expression: ScalarExpr::InternalColumn(raw.column(position))
                        .copy_for_rewrite(control)?,
                    alias: Some(control.copy_text(name)?),
                },
                control,
            )?;
        }
        let filter = query
            .copy_for_rewrite(control)?
            .set_analyzed_output(None, control)?
            .discard_source_analysis(control)?
            .replace_source(Some(self.source), control)?
            .replace_where_expression(predicate, control)?
            .clear_privilege_columns(control)?
            .replace_projections(projections.into_iter(), control)?;
        self.source = source(filter, filtered, control)?;
        Ok(self)
    }
}
