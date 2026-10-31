//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed wildcard observation owns SQL namespace validation and admitted layout scratch.

use crate::{RowSchema, SQLError, ScalarExpr};
use uqa_core::memory::ProductionControl;

/// Generate a reference to one bound logical position. Ambiguous names retain an ordinal reference; uniquely addressable names preserve their original qualifier.
pub fn bound_projection_expression_with_control(
    schema: &RowSchema,
    position: usize,
    control: &ProductionControl<'_>,
) -> Result<crate::plan::OwnedScalarExpr, SQLError> {
    if let Some(identity) = schema.identity(position) {
        let unique = identity.qualifier().map_or_else(
            || schema.unqualified_position(identity.column()) == Some(position),
            |qualifier| schema.qualified_position(qualifier, identity.column()) == Some(position),
        );
        if unique {
            return Ok(crate::plan::OwnedScalarExpr::column(
                control.copy_text(identity.column())?,
                identity
                    .qualifier()
                    .map(|name| control.copy_text(name))
                    .transpose()?,
                control,
            )?);
        }
    }
    Ok(ScalarExpr::Position(position).copy_for_rewrite(control)?)
}

/// Expand a qualified ROW star while retaining each generated scalar independently from the admitted result buffer.
pub(crate) fn row_star_fields_with_control(
    expression: &ScalarExpr,
    schema: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<Vec<crate::plan::OwnedScalarExpr>>>, SQLError> {
    control.check()?;
    if !matches!(expression, ScalarExpr::QualifiedStar(_)) {
        return Ok(None);
    }
    let mut fields = uqa_core::memory::ProductionVec::new(*control);
    visit_bound_projection_star_fields(
        expression,
        schema,
        schema,
        control,
        &mut |column, position, qualifier| {
            let expression = match position {
                Some(position) => {
                    bound_projection_expression_with_control(schema, position, control)?
                }
                None => crate::plan::OwnedScalarExpr::column(
                    control.copy_text(column)?,
                    Some(control.copy_text(qualifier.expect("hidden qualified field"))?),
                    control,
                )?,
            };
            fields.push_produced(control.finish(expression, control.empty_reservation())?)?;
            Ok(())
        },
    )?;
    Ok(Some(fields.finish()?))
}

pub type ProjectionFieldVisitor<'a> =
    dyn FnMut(&str, Option<usize>, Option<&str>) -> Result<(), SQLError> + 'a;

/// Visit each visible field of a bound wildcard. Bare stars use the local FROM output; qualified stars preserve the selected enclosing namespace, LATERAL validation and physical field order. A non-wildcard returns false without invoking the consumer.
pub fn visit_bound_projection_star_fields(
    expression: &ScalarExpr,
    schema: &RowSchema,
    expression_schema: &RowSchema,
    control: &ProductionControl<'_>,
    observe: &mut ProjectionFieldVisitor<'_>,
) -> Result<bool, SQLError> {
    visit_bound_projection_star_metadata(
        expression,
        schema,
        expression_schema,
        control,
        &mut |field| observe(field.name, field.logical, field.qualifier),
    )
}

pub struct ProjectionStarMetadata<'a> {
    pub name: &'a str,
    pub logical: Option<usize>,
    pub qualifier: Option<&'a str>,
    pub ty: Option<&'a crate::ColumnType>,
    pub collation: Option<u32>,
    pub(crate) record: Option<&'a crate::schema::RecordMetadata>,
}

pub fn visit_bound_projection_star_metadata<'a>(
    expression: &'a ScalarExpr,
    schema: &'a RowSchema,
    expression_schema: &'a RowSchema,
    control: &ProductionControl<'_>,
    observe: &mut dyn FnMut(ProjectionStarMetadata<'a>) -> Result<(), SQLError>,
) -> Result<bool, SQLError> {
    control.check()?;
    match expression {
        ScalarExpr::Star => {
            for (position, column) in schema.columns().iter().enumerate() {
                control.check()?;
                if super::visible_projection_source_position(schema, position) {
                    observe(ProjectionStarMetadata {
                        name: schema.public_name(position).unwrap_or(column),
                        logical: Some(position),
                        qualifier: None,
                        ty: schema.column_type(position),
                        collation: schema.column_collation(position),
                        record: schema.record_metadata(position),
                    })?;
                }
            }
        }
        ScalarExpr::QualifiedStar(qualifier) => {
            let schema = expression_schema;
            schema.require_lateral_qualifier(qualifier)?;
            let mut found = false;
            schema.visit_qualified_star_metadata(
                qualifier,
                control,
                |name, logical, ty, collation, record| {
                    found = true;
                    if logical.is_none_or(|position| {
                        super::visible_projection_source_position(schema, position)
                    }) {
                        observe(ProjectionStarMetadata {
                            name,
                            logical,
                            qualifier: Some(qualifier),
                            ty,
                            collation,
                            record,
                        })?;
                    }
                    Ok(())
                },
            )?;
            if !found && !schema.has_qualifier(qualifier) {
                return Err(schema
                    .unavailable_qualifier_error(qualifier)
                    .unwrap_or_else(|| SQLError::UnknownTable(qualifier.to_string())));
            }
        }
        _ => return Ok(false),
    }
    control.check()?;
    Ok(true)
}
