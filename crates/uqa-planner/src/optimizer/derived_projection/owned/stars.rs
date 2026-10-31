//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Wildcard expansion moves surviving projection owners and admits only generated references and containers.

use super::{
    Buffer, MutableQueryPlan, OwnedScalarExpr, ProductionControl, RelationalPlan, RowSchema,
    SQLError, ScalarExpr,
};
use uqa_sql::plan::OwnedProjection;

pub(crate) fn expand(
    query: MutableQueryPlan,
    schema: Option<&RowSchema>,
    control: &ProductionControl<'_>,
) -> Result<(MutableQueryPlan, bool), SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    if !block.projections.iter().any(|projection| {
        matches!(
            projection.expr,
            ScalarExpr::Star | ScalarExpr::QualifiedStar(_)
        )
    }) {
        return Ok((query, true));
    }
    let Some(source) = query.analyzed_source.clone() else {
        return Ok((query, false));
    };
    if schema.is_some_and(|schema| schema.columns_are_open(None))
        || block
            .projections
            .iter()
            .any(|projection| match &projection.expr {
                ScalarExpr::Star => source.schema.columns_are_open(None),
                ScalarExpr::QualifiedStar(qualifier) => {
                    source.schema.columns_are_open(Some(qualifier))
                }
                _ => false,
            })
    {
        return Ok((query, false));
    }
    let mut count = 0;
    for projection in &block.projections {
        let counted = observe_star(&projection.expr, &source.schema, control, &mut |_, _, _| {
            count += 1;
            Ok(())
        });
        match counted {
            Ok(false) => count += 1,
            Ok(true) => {}
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
            Err(_) => return Ok((query, false)),
        }
    }
    if schema.is_none_or(|schema| schema.len() != count) {
        return Ok((query, false));
    }
    let (query, projections) = query.take_projections(control)?;
    let mut expanded = Buffer::new(*control);
    for projection in projections {
        let star = observe_star(
            &projection.expression,
            &source.schema,
            control,
            &mut |column, position, qualifier| {
                let expression = match position {
                    Some(position) => uqa_sql::semantics::bound_projection_expression_with_control(
                        &source.schema,
                        position,
                        control,
                    )?,
                    None => OwnedScalarExpr::column(
                        control.copy_text(column)?,
                        qualifier
                            .map(|qualifier| control.copy_text(qualifier))
                            .transpose()?,
                        control,
                    )?,
                };
                expanded.push(OwnedProjection {
                    expression,
                    alias: Some(control.copy_text(column)?),
                })
            },
        )?;
        if !star {
            expanded.push(projection)?;
        }
    }
    Ok((
        query.replace_projections(expanded.into_iter(), control)?,
        true,
    ))
}

type ProjectionFieldObserver<'a> =
    dyn FnMut(&str, Option<usize>, Option<&str>) -> Result<(), SQLError> + 'a;

fn observe_star(
    expression: &ScalarExpr,
    schema: &RowSchema,
    control: &ProductionControl<'_>,
    observe: &mut ProjectionFieldObserver<'_>,
) -> Result<bool, SQLError> {
    uqa_sql::semantics::visit_bound_projection_star_fields(
        expression, schema, schema, control, observe,
    )
}
