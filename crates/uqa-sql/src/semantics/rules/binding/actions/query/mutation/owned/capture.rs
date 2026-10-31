//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured mutation sources preserve hidden physical aliases and canonical RETURNING context slots.

use super::{input_expression, CapturedSource, RuleMutation};
use crate::{
    ast::{ColumnDef, Expr, InternalRelationId},
    catalog::roles::RoleReference,
    plan::{
        MutableExpressionPlan, MutableQueryPlan, MutableSourcePlan, OwnedProjection,
        OwnedScalarExpr, ScalarQueryArena, SourcePlan,
    },
    production::Buffer,
    RowSchema, SQLError, ScalarExpr,
};
use uqa_core::memory::{OwnedMap, ProductionControl, ProductionMap, ProductionVec};

pub(super) fn source(
    statement: RuleMutation<'_>,
    source: &SourcePlan,
    schema: &RowSchema,
    relation: InternalRelationId,
    subject: &RoleReference,
    control: &ProductionControl<'_>,
) -> Result<CapturedSource, SQLError> {
    let mut projections = Buffer::new(control);
    for (position, name) in schema.columns().iter().enumerate() {
        projections.push(
            OwnedProjection {
                expression: ScalarExpr::Position(position).copy_for_rewrite(control)?,
                alias: Some(control.copy_text(name)?),
            },
            control,
        )?;
    }
    for (qualifier, ()) in &whole_rows(schema, *control)? {
        projections.push(
            OwnedProjection {
                expression: OwnedScalarExpr::column(control.copy_text(qualifier)?, None, control)?,
                alias: None,
            },
            control,
        )?;
    }
    virtual_projections(schema, &mut projections, control)?;
    let mut slots = ProductionMap::new(*control);
    for position in 0..schema.len() {
        if let Some(slot) = schema.physical_slot(position) {
            slots.insert_copy(slot, position)?;
        }
    }
    for (identity, slot) in schema.lookup_aliases() {
        if let Some(slot) = slot {
            if slots.get(&slot).is_none() {
                slots.insert_copy(slot, projections.len())?;
                projections.push(
                    OwnedProjection {
                        expression: column(identity.qualifier(), identity.column(), control)?,
                        alias: None,
                    },
                    control,
                )?;
            }
        }
    }
    let mut columns = OwnedMap::new();
    for (column, slot) in schema.internal_identities() {
        let position = if let Some(position) = slots.get(&slot) {
            *position
        } else {
            let position = projections.len();
            slots.insert_copy(slot, position)?;
            projections.push(
                OwnedProjection {
                    expression: ScalarExpr::InternalColumn(column).copy_for_rewrite(control)?,
                    alias: None,
                },
                control,
            )?;
            position
        };
        super::super::super::resolver::insert_expression(
            &mut columns,
            column,
            input_expression(relation, position, control)?,
            control,
        )?;
    }
    let mut block = MutableQueryPlan::projected(projections.into_iter(), control)?
        .replace_source(Some(source.copy_for_rewrite(control)?), control)?;
    block = block.map_projection_inputs(control, |projections, arena| {
        arena.append_arena(
            ScalarQueryArena::copy_from(statement.subqueries(), control)?,
            control,
        )?;
        Ok(projections)
    })?;
    let (schema, context_slots) = schema.canonical_projection_with_control(control)?;
    let mut context = ProductionVec::new(*control);
    for slot in context_slots.iter() {
        let position = *slots
            .get(slot)
            .expect("canonical source slot has a captured projection");
        context.push_produced(control.finish(
            Expr::InternalColumn(relation.column(position)),
            control.empty_reservation(),
        )?)?;
    }
    Ok(CapturedSource {
        source: captured(
            query(block, statement, subject, control)?,
            relation,
            control,
        )?,
        columns,
        context: context.finish()?,
        schema,
    })
}

pub(super) fn target(
    statement: RuleMutation<'_>,
    definitions: &[ColumnDef],
    schema: &RowSchema,
    relation: InternalRelationId,
    subject: &RoleReference,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    let mut projections = Buffer::new(control);
    for definition in definitions {
        projections.push(
            OwnedProjection {
                expression: column(
                    Some(statement.target_qualifier()),
                    &definition.name,
                    control,
                )?,
                alias: Some(control.copy_text(&definition.name)?),
            },
            control,
        )?;
    }
    projections.push(
        OwnedProjection {
            expression: column(None, statement.target_qualifier(), control)?,
            alias: None,
        },
        control,
    )?;
    virtual_projections(schema, &mut projections, control)?;
    let block = MutableQueryPlan::projected(projections.into_iter(), control)?.replace_source(
        Some(statement.target_source_with_control(control)?),
        control,
    )?;
    captured(
        query(block, statement, subject, control)?,
        relation,
        control,
    )
}

pub(super) fn action_query(
    statement: RuleMutation<'_>,
    target: Option<MutableSourcePlan>,
    source: Option<MutableSourcePlan>,
    predicate: Option<MutableExpressionPlan>,
    subject: &RoleReference,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let source = match (target, source) {
        (Some(target), Some(source)) => Some(MutableSourcePlan::cross_join(
            source, target, true, control,
        )?),
        (target, source) => target.or(source),
    };
    let mut query = query(
        MutableQueryPlan::projected(std::iter::empty(), control)?
            .replace_source(source, control)?,
        statement,
        subject,
        control,
    )?;
    if let Some(predicate) = predicate {
        query = query.map_where_input(control, |_, arena| {
            Ok(Some(arena.append_expression(predicate, control)?))
        })?;
    }
    Ok(query)
}

pub(super) fn query(
    query: MutableQueryPlan,
    statement: RuleMutation<'_>,
    subject: &RoleReference,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    query
        .set_relations_bound(statement.relations_bound(), control)?
        .set_privilege_subject(Some(subject.clone_with_control(control)?), control)
}

fn captured(
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

fn column(
    qualifier: Option<&str>,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    Ok(OwnedScalarExpr::column(
        control.copy_text(name)?,
        qualifier.map(|name| control.copy_text(name)).transpose()?,
        control,
    )?)
}

fn virtual_projections(
    schema: &RowSchema,
    projections: &mut Buffer<OwnedProjection>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for (identity, _) in schema.typed_virtual_identities() {
        if identity.qualifier().is_some() {
            projections.push(
                OwnedProjection {
                    expression: column(identity.qualifier(), identity.column(), control)?,
                    alias: None,
                },
                control,
            )?;
        }
    }
    Ok(())
}

pub(super) fn whole_rows<'s, 'c>(
    schema: &'s RowSchema,
    control: ProductionControl<'c>,
) -> Result<ProductionMap<'c, &'s str, ()>, SQLError> {
    let mut qualifiers = ProductionMap::new(control);
    for identity in schema.identities() {
        if let Some(qualifier) = identity.qualifier() {
            qualifiers.insert_copy(qualifier, ())?;
        }
    }
    Ok(qualifiers)
}
