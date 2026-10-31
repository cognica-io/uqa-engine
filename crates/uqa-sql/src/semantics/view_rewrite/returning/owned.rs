//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Automatic RETURNING expansion transfers scalar owners and admits each new name, query and projection buffer.

use super::super::{
    AutomaticViewLayer, ExpressionScope, RowSchema, SQLError, ScalarExpr, ViewRewriteContext,
};
use crate::{
    plan::{OwnedProjection, OwnedScalarExpr, ScalarQueryArena},
    production::Buffer,
    semantics::view_rewrite::{layer_column, layer_rewrite::embed_owned as embed},
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(in crate::semantics::view_rewrite) fn target_expression(
    services: ViewRewriteContext<'_>,
    input: OwnedScalarExpr,
    layer: &AutomaticViewLayer,
    scope: ExpressionScope<'_>,
    arena: &mut ScalarQueryArena,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    input.replace_nodes(control, &mut |node| {
        let (column, qualifier) = match node {
            ScalarExpr::Column(column) => (column, scope.target_qualifier),
            ScalarExpr::QualifiedColumn { qualifier, column }
                if scope.target_qualifier(qualifier) =>
            {
                (column, qualifier.as_str())
            }
            _ => return Ok(None),
        };
        if let Some(mapping) = layer_column(layer, column) {
            return Ok(Some(arena.append_expression(
                embed(services, &mapping.expression, layer, qualifier, control)?,
                control,
            )?));
        }
        if matches!(node, ScalarExpr::Column(_)) {
            if let Some(source) = scope.source {
                if let Some(qualifier) = source
                    .unqualified_position(column)
                    .and_then(|position| source.identity(position))
                    .and_then(crate::ColumnIdentity::qualifier)
                {
                    return Ok(Some(OwnedScalarExpr::positioned_column(
                        control.copy_text(column)?,
                        Some(control.copy_text(qualifier)?),
                        column.position(),
                        control,
                    )?));
                }
            }
        }
        Ok(None)
    })
}

pub(in crate::semantics::view_rewrite) fn returning(
    services: ViewRewriteContext<'_>,
    input: impl Iterator<Item = OwnedProjection>,
    layer: &AutomaticViewLayer,
    scope: ExpressionScope<'_>,
    source_first: bool,
    arena: &mut ScalarQueryArena,
    control: &ProductionControl<'_>,
) -> Result<(Buffer<OwnedProjection>, Produced<Vec<usize>>), SQLError> {
    let mut output = Buffer::new(control);
    let mut boundaries = ProductionVec::new(*control);
    for projection in input {
        control.check()?;
        let bare = matches!(&*projection.expression, ScalarExpr::Star);
        let qualifier = match &*projection.expression {
            ScalarExpr::Star => Some(scope.target_qualifier),
            ScalarExpr::QualifiedStar(qualifier) if scope.target_qualifier(qualifier) => {
                Some(qualifier.as_str())
            }
            _ => None,
        };
        if let Some(qualifier) = qualifier {
            if bare && source_first && scope.source.is_some() {
                boundaries.push_copy(output.len())?;
            }
            for column in &layer.columns {
                control.check()?;
                output.push(
                    OwnedProjection {
                        expression: arena.append_expression(
                            embed(services, &column.expression, layer, qualifier, control)?,
                            control,
                        )?,
                        alias: Some(control.copy_text(&column.name)?),
                    },
                    control,
                )?;
            }
            if bare && !source_first && scope.source.is_some() {
                boundaries.push_copy(output.len())?;
            }
            continue;
        }
        let alias = match projection.alias {
            Some(alias) => Some(alias),
            None => match &*projection.expression {
                ScalarExpr::Column(column) if layer_column(layer, column).is_some() => {
                    Some(control.copy_text(column)?)
                }
                ScalarExpr::QualifiedColumn { qualifier, column }
                    if scope.target_qualifier(qualifier)
                        && layer_column(layer, column).is_some() =>
                {
                    Some(control.copy_text(column)?)
                }
                _ => None,
            },
        };
        output.push(
            OwnedProjection {
                expression: target_expression(
                    services,
                    projection.expression,
                    layer,
                    scope,
                    arena,
                    control,
                )?,
                alias,
            },
            control,
        )?;
    }
    Ok((output, boundaries.finish()?))
}

pub(in crate::semantics::view_rewrite) fn target_width(
    services: ViewRewriteContext<'_>,
    table: &str,
    control: &ProductionControl<'_>,
) -> Result<usize, SQLError> {
    let columns = super::super::relation_columns_with_control(services, table, control)?;
    Ok(columns.len()
        + usize::from(
            !columns
                .iter()
                .any(|name| name == crate::semantics::DOC_ID_COLUMN),
        )
        + 2)
}

pub(in crate::semantics::view_rewrite) fn source_positions(
    input: OwnedScalarExpr,
    source: &RowSchema,
    width: usize,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    input.replace_nodes(control, &mut |node| {
        let ScalarExpr::Column(column) = node else {
            return Ok(None);
        };
        let position = source.unqualified_position(column).filter(|position| {
            source
                .identity(*position)
                .is_some_and(|identity| identity.qualifier().is_none())
        });
        Ok(position
            .map(|position| ScalarExpr::Position(width + position).copy_for_rewrite(control))
            .transpose()?)
    })
}

pub(in crate::semantics::view_rewrite) fn finalize(
    services: ViewRewriteContext<'_>,
    table: &str,
    input: impl Iterator<Item = OwnedProjection>,
    source: Option<&RowSchema>,
    boundaries: &[usize],
    control: &ProductionControl<'_>,
) -> Result<Buffer<OwnedProjection>, SQLError> {
    let mut output = Buffer::new(control);
    let Some(source) = source else {
        for projection in input {
            output.push(projection, control)?;
        }
        return Ok(output);
    };
    let width = target_width(services, table, control)?;
    let mut boundaries = boundaries.iter().copied().peekable();
    let mut count = 0;
    for (index, projection) in input.enumerate() {
        control.check()?;
        while boundaries.peek() == Some(&index) {
            append_source_star(&mut output, source, width, control)?;
            boundaries.next();
        }
        expand_projection(&mut output, projection, source, width, control)?;
        count = index + 1;
    }
    while boundaries.peek() == Some(&count) {
        append_source_star(&mut output, source, width, control)?;
        boundaries.next();
    }
    assert!(
        boundaries.next().is_none(),
        "RETURNING boundary belongs to the original projection list"
    );
    Ok(output)
}

fn append_source_star(
    output: &mut Buffer<OwnedProjection>,
    source: &RowSchema,
    width: usize,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for (position, name) in source.columns().iter().enumerate() {
        control.check()?;
        if source.wildcard_position_visible(position) {
            output.push(
                OwnedProjection {
                    expression: ScalarExpr::Position(width + position).copy_for_rewrite(control)?,
                    alias: Some(control.copy_text(source.public_name(position).unwrap_or(name))?),
                },
                control,
            )?;
        }
    }
    Ok(())
}

pub(super) fn expand_projection(
    output: &mut Buffer<OwnedProjection>,
    projection: OwnedProjection,
    source: &RowSchema,
    width: usize,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let ScalarExpr::QualifiedStar(qualifier) = &*projection.expression else {
        return output.push(projection, control);
    };
    let mut found = false;
    source.visit_qualified_star_positions(qualifier, control, |name, logical, _, _| {
        found = true;
        if logical.is_some_and(|position| !source.wildcard_position_visible(position)) {
            return Ok(());
        }
        let expression = match logical {
            Some(position) => ScalarExpr::Position(width + position).copy_for_rewrite(control)?,
            None => OwnedScalarExpr::column(
                control.copy_text(name)?,
                Some(control.copy_text(qualifier)?),
                control,
            )?,
        };
        output.push(
            OwnedProjection {
                expression,
                alias: Some(control.copy_text(name)?),
            },
            control,
        )
    })?;
    if !found && !source.has_qualifier(qualifier) {
        return Err(SQLError::UnknownTable(qualifier.to_string()));
    }
    Ok(())
}

pub(in crate::semantics::view_rewrite) fn combine_predicate(
    services: ViewRewriteContext<'_>,
    current: Option<OwnedScalarExpr>,
    layer: &AutomaticViewLayer,
    qualifier: &str,
    arena: &mut ScalarQueryArena,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    let view = layer
        .predicate
        .as_ref()
        .map(|predicate| {
            arena.append_expression(
                embed(services, predicate, layer, qualifier, control)?,
                control,
            )
        })
        .transpose()?;
    Ok(match (view, current) {
        (Some(view), Some(current)) => Some(OwnedScalarExpr::boolean(
            [view, current].into_iter(),
            true,
            control,
        )?),
        (Some(view), None) => Some(view),
        (None, current) => current,
    })
}
