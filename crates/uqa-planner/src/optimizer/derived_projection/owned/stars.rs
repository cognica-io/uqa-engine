//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve wildcards against the original namespace and move every other projection directly to its output slot.

use super::{
    Buffer, MutableQueryPlan, OwnedScalarExpr, ProductionControl, RelationalPlan, RowSchema,
    SQLError, ScalarExpr,
};
use crate::{ProjectionPlan, QueryBlockPlan};
use uqa_core::memory::ProductionVec;
use uqa_sql::plan::{AnalyzedSource, OwnedProjection};

enum Expansion<T> {
    Unchanged,
    Unresolved,
    Fields(Buffer<Field<T>>),
}

enum Field<T> {
    Original(usize),
    Generated(T),
}

/// Generated fields precede publication. An original slot is consumed only when the result reaches it.
fn resolve<P, T>(
    fields: Buffer<Field<T>>,
    mut originals: impl Iterator<Item = P>,
    mut publish: impl FnMut(T) -> P,
) -> impl ExactSizeIterator<Item = P> {
    let mut next = 0;
    fields.into_iter().map(move |field| match field {
        Field::Original(index) => {
            let original = originals
                .nth(index - next)
                .expect("selected original projection");
            next = index + 1;
            original
        }
        Field::Generated(field) => publish(field),
    })
}

pub(crate) fn expand(
    query: MutableQueryPlan,
    schema: Option<&RowSchema>,
    control: &ProductionControl<'_>,
) -> Result<(MutableQueryPlan, bool), SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    match prepare(
        &block.projections,
        query.analyzed_source.as_deref(),
        schema,
        control,
        Ok,
    )? {
        Expansion::Unchanged => Ok((query, true)),
        Expansion::Unresolved => Ok((query, false)),
        Expansion::Fields(fields) => {
            let (query, originals) = query.take_projections(control)?;
            let expanded = resolve(fields, originals, |projection| projection);
            Ok((query.replace_projections(expanded, control)?, true))
        }
    }
}

pub(crate) fn expand_caller(
    block: &mut QueryBlockPlan,
    source: Option<&AnalyzedSource>,
    schema: Option<&RowSchema>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    match prepare(&block.projections, source, schema, control, |field| {
        field.finish(control)
    })? {
        Expansion::Unchanged => Ok(true),
        Expansion::Unresolved => Ok(false),
        Expansion::Fields(fields) => {
            // Admit the complete destination while the input is still intact. Publication moves the caller's existing fields and transfers the new allocations to the caller.
            let mut destination = ProductionVec::new(*control);
            destination.reserve(fields.len())?;
            let (mut expanded, memory) = destination.finish()?.into_parts();
            let originals = std::mem::take(&mut block.projections);
            expanded.extend(resolve(fields, originals.into_iter(), |field| {
                field.into_parts().0
            }));
            block.projections = expanded;
            drop(memory);
            Ok(true)
        }
    }
}

fn prepare<T>(
    projections: &[ProjectionPlan],
    source: Option<&AnalyzedSource>,
    output: Option<&RowSchema>,
    control: &ProductionControl<'_>,
    mut finish: impl FnMut(OwnedProjection) -> Result<T, SQLError>,
) -> Result<Expansion<T>, SQLError> {
    if !projections
        .iter()
        .any(|p| matches!(p.expr, ScalarExpr::Star | ScalarExpr::QualifiedStar(_)))
    {
        return Ok(Expansion::Unchanged);
    }
    let Some(source) = source else {
        return Ok(Expansion::Unresolved);
    };
    let schema = &source.schema;
    if output.is_some_and(|schema| schema.columns_are_open(None))
        || projections.iter().any(|p| match &p.expr {
            ScalarExpr::Star => schema.columns_are_open(None),
            ScalarExpr::QualifiedStar(qualifier) => schema.columns_are_open(Some(qualifier)),
            _ => false,
        })
    {
        return Ok(Expansion::Unresolved);
    }
    let mut width = 0;
    for projection in projections {
        match observe_star(&projection.expr, schema, control, &mut |_, _, _| {
            width += 1;
            Ok(())
        }) {
            Ok(false) => width += 1,
            Ok(true) => {}
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
            Err(_) => return Ok(Expansion::Unresolved),
        }
    }
    if output.is_none_or(|schema| schema.len() != width) {
        return Ok(Expansion::Unresolved);
    }
    let mut fields = Buffer::new(*control);
    for (index, projection) in projections.iter().enumerate() {
        let star = observe_star(
            &projection.expr,
            schema,
            control,
            &mut |column, position, qualifier| {
                let expression = match position {
                    Some(position) => uqa_sql::semantics::bound_projection_expression_with_control(
                        schema, position, control,
                    )?,
                    None => OwnedScalarExpr::column(
                        control.copy_text(column)?,
                        qualifier.map(|name| control.copy_text(name)).transpose()?,
                        control,
                    )?,
                };
                let field = OwnedProjection {
                    expression,
                    alias: Some(control.copy_text(column)?),
                };
                fields.push(Field::Generated(finish(field)?))
            },
        )?;
        if !star {
            fields.push(Field::Original(index))?;
        }
    }
    Ok(Expansion::Fields(fields))
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
