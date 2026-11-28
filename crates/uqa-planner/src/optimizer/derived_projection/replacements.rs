//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select and construct discarded projection values while the original query remains available to every catalog observation.

use super::{observations, OutputPruningContext, QueryPlan, RelationalPlan, RowSchema, SQLError};
use crate::scratch::Buffer;
use uqa_core::{memory::Produced, Value};
use uqa_sql::plan::OwnedScalarExpr;

pub(in crate::optimizer) struct Replacement<T> {
    pub(super) index: usize,
    pub(super) expression: T,
    pub(super) alias: Option<Produced<String>>,
}

pub(super) fn collect<T>(
    query: &QueryPlan,
    schema: Option<&RowSchema>,
    required: &dyn Fn(usize) -> bool,
    context: OutputPruningContext<'_>,
    mut finish: impl FnMut(OwnedScalarExpr) -> Result<T, SQLError>,
) -> Result<Buffer<Replacement<T>>, SQLError> {
    let RelationalPlan::QueryBlock(inner) = &query.root else {
        unreachable!("projection replacements require a query block")
    };
    let control = &context.production;
    let visible_ctes =
        |name: &str| query.ctes.iter().any(|cte| cte.name == name) || (context.visible_ctes)(name);
    let mut replacements = Buffer::new(*control);
    for (index, projection) in inner.projections.iter().enumerate() {
        control.check()?;
        if required(index)
            || observations::needed_by_ordering(inner, index, control)?
            || observations::needed_by_grouping(
                inner,
                index,
                query.analyzed_source.as_deref(),
                control,
            )?
            || !observations::removable(
                &projection.expr,
                &inner.subqueries,
                context,
                &visible_ctes,
            )?
        {
            continue;
        }
        let ty = match schema.and_then(|schema| schema.column_type(index)) {
            Some(ty) => Some(ty.clone_with_control(control)?),
            None => match uqa_sql::scalar_type_with_resolver_and_control(
                &projection.expr,
                &RowSchema::default(),
                &[],
                None,
                control,
            ) {
                Ok(ty) => ty,
                Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                    return Err(error);
                }
                Err(_) => None,
            },
        };
        if let Some(ty) = ty {
            let expression = OwnedScalarExpr::typed_literal(
                control.finish(Value::Null, control.empty_reservation())?,
                ty.catalog_name_with_control(control)?,
                Some(ty),
                schema.and_then(|schema| schema.column_collation(index)),
                None,
                control,
            )?;
            replacements.push(Replacement {
                index,
                expression: finish(expression)?,
                alias: None,
            })?;
        }
    }
    // Complete catalog and type observation before deriving labels for the selected substitutions.
    for replacement in replacements.iter_mut() {
        let projection = &inner.projections[replacement.index];
        if projection.alias.is_none() {
            replacement.alias = Some(uqa_sql::semantics::with_projection_label(
                projection,
                control,
                |name| Ok(control.copy_text(name)?),
            )?);
        }
    }
    Ok(replacements)
}
