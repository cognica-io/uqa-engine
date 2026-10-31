//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unused outputs retain positional metadata while retiring their scalar owners immediately.

use super::{observations, OutputPruningContext, RelationalPlan, RowSchema, SQLError, ScalarExpr};
use crate::scratch::Buffer;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_sql::plan::{MutableQueryPlan, OwnedScalarExpr, QueryScalarSlot};

mod stars;
pub(crate) use stars::expand as expand_stars;
#[cfg(test)]
mod tests;

pub fn prune_outputs(
    query: MutableQueryPlan,
    aliases: &[String],
    required: &dyn Fn(&str) -> bool,
    schema: Option<&RowSchema>,
    context: OutputPruningContext<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let Some(demanded) =
        super::output_demand(&query, aliases, required, schema, context.production)?
    else {
        return Ok(query);
    };
    prune_positions(
        query,
        &|position| demanded.get(position).copied().unwrap_or(true),
        schema,
        context,
    )
}

pub(in crate::optimizer) fn prune_positions(
    mut query: MutableQueryPlan,
    required: &dyn Fn(usize) -> bool,
    schema: Option<&RowSchema>,
    context: OutputPruningContext<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let control = &context.production;
    control.check()?;
    if query.analyzed_output.is_none() {
        if let Some(schema) = schema {
            query = query.set_analyzed_output(Some(schema.clone()), control)?;
            if let RelationalPlan::QueryBlock(block) = &query.root {
                let compute = block.compute_kind_with_control(context.aggregates, control)?;
                query = query.set_compute(compute, control)?;
            }
        }
    }
    if matches!(query.root, RelationalPlan::SetOp { .. }) {
        return super::super::union_projection::owned::prune(query, required, context);
    }
    let RelationalPlan::QueryBlock(inner) = &query.root else {
        return Ok(query);
    };
    if inner.distinct || !inner.distinct_on.is_empty() || !inner.windows.is_empty() {
        return Ok(query);
    }
    let schema = schema.cloned().or_else(|| query.analyzed_output.clone());
    let (mut query, expanded) = stars::expand(query, schema.as_ref(), control)?;
    if !expanded {
        return Ok(query);
    }
    let RelationalPlan::QueryBlock(inner) = &query.root else {
        unreachable!()
    };
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
        let ty = match schema.as_ref().and_then(|schema| schema.column_type(index)) {
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
                schema
                    .as_ref()
                    .and_then(|schema| schema.column_collation(index)),
                None,
                control,
            )?;
            replacements.push((index, expression))?;
        }
    }
    for (index, expression) in replacements {
        query = replace(query, index, expression, control)?;
    }
    Ok(query)
}

fn replace(
    mut query: MutableQueryPlan,
    index: usize,
    expression: OwnedScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    let projection = &block.projections[index];
    let mut privileges: Buffer<Produced<uqa_sql::ColumnIdentity>> = Buffer::new(*control);
    projection.expr.try_visit(&mut |part| {
        control.check()?;
        match part {
            ScalarExpr::Column(column) => privileges.push(
                uqa_sql::ColumnIdentity::from_parts_with_control(None, column, control)?,
            )?,
            ScalarExpr::QualifiedColumn { qualifier, column } => privileges.push(
                uqa_sql::ColumnIdentity::from_parts_with_control(Some(qualifier), column, control)?,
            )?,
            _ => {}
        }
        Ok::<_, SQLError>(true)
    })?;
    let alias = if projection.alias.is_none() {
        Some(uqa_sql::semantics::with_projection_label(
            projection,
            control,
            |name| Ok(control.copy_text(name)?),
        )?)
    } else {
        None
    };
    for identity in privileges {
        query = query.include_privilege_column(identity, control)?;
    }
    if let Some(alias) = alias {
        query = query.replace_projection_alias(index, Some(alias), control)?;
    }
    query.map_scalar(QueryScalarSlot::Projection(index), control, |_| {
        Ok(expression)
    })
}

pub(in crate::optimizer) fn prune(
    query: MutableQueryPlan,
    context: OutputPruningContext<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(query);
    };
    let Some(demanded) = super::demand::consumer_outputs(block, context.production)? else {
        return Ok(query);
    };
    query.map_source(&context.production, |source| {
        source.map_query(&context.production, |body| {
            prune_positions(
                body,
                &|position| demanded.get(position).copied().unwrap_or(true),
                None,
                context,
            )
        })
    })
}
