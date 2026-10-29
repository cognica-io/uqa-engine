//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source constant observation shares semantic selection while admitting each produced name and scalar destination.

use super::{
    ColumnType, QueryBlockPlan, RelationalPlan, RoutineTypeCatalog, SQLError, ScalarExpr,
    SourcePlan,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_sql::{
    plan::{OwnedScalarExpr, QueryPlan},
    semantics::volatility::VolatilityCatalog,
};

#[cfg(test)]
mod tests;

type Observer<'a> =
    dyn FnMut(Produced<String>, Option<OwnedScalarExpr>) -> Result<(), SQLError> + 'a;

pub(super) fn observe_source(
    source: &SourcePlan,
    types: Option<&dyn RoutineTypeCatalog>,
    views: Option<&dyn VolatilityCatalog>,
    control: &ProductionControl<'_>,
    observe: &mut Observer<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    match source {
        SourcePlan::Table {
            name,
            bound_columns,
            column_aliases,
            ..
        } => {
            let Some(views) = views else { return Ok(false) };
            Ok(views
                .with_view_query_with_control(name, control, &mut |body| {
                    observe_query(
                        body,
                        |index| {
                            column_aliases.get(index).or_else(|| {
                                bound_columns.as_ref().and_then(|names| names.get(index))
                            })
                        },
                        types,
                        control,
                        observe,
                    )
                })?
                .unwrap_or(false))
        }
        SourcePlan::Values {
            rows,
            column_aliases,
            internal_relation: None,
            ..
        } if rows.len() == 1 => {
            observe_values(
                &rows[0],
                |index| value_column_name(column_aliases.get(index), index, control),
                types,
                control,
                observe,
            )?;
            Ok(true)
        }
        SourcePlan::Subquery {
            captured_output: None,
            body,
            column_aliases,
            ..
        } => observe_query(
            body,
            |index| column_aliases.get(index),
            types,
            control,
            observe,
        ),
        _ => Ok(false),
    }
}

fn observe_query<'a>(
    body: &QueryPlan,
    column_name: impl Fn(usize) -> Option<&'a String>,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
    observe: &mut Observer<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    if !body.ctes.is_empty() {
        return Ok(false);
    }
    match &body.root {
        RelationalPlan::Values { rows, subqueries } if rows.len() == 1 && subqueries.is_empty() => {
            observe_values(
                &rows[0],
                |index| value_column_name(column_name(index), index, control),
                types,
                control,
                observe,
            )?;
            Ok(true)
        }
        RelationalPlan::QueryBlock(inner) if inner.from.is_none() => {
            for projection in &inner.projections {
                if !constant_literal(&projection.expr, types, control)? {
                    return Ok(false);
                }
            }
            observe_values(
                inner.projections.iter().map(|projection| &projection.expr),
                |index| match column_name(index) {
                    Some(name) => Ok(control.copy_text(name)?),
                    None => uqa_sql::semantics::with_projection_label(
                        &inner.projections[index],
                        control,
                        |name| Ok(control.copy_text(name)?),
                    ),
                },
                types,
                control,
                observe,
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn value_column_name(
    name: Option<&String>,
    index: usize,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    Ok(match name {
        Some(name) => control.copy_text(name)?,
        None => control.format(format_args!("column{}", index + 1))?,
    })
}

pub(super) fn observe_values<'a>(
    values: impl IntoIterator<Item = &'a ScalarExpr>,
    column_name: impl Fn(usize) -> Result<Produced<String>, SQLError>,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
    observe: &mut Observer<'_>,
) -> Result<(), SQLError> {
    for (index, value) in values.into_iter().enumerate() {
        control.check()?;
        let name = column_name(index)?;
        let selected = if constant_literal(value, types, control)? {
            Some(match value {
                ScalarExpr::Literal(value @ (Value::Null | Value::Str(_))) => {
                    OwnedScalarExpr::typed_literal(
                        control.copy_value(value)?,
                        control.copy_text("text")?,
                        Some(ColumnType::Text.clone_with_control(control)?),
                        None,
                        None,
                        control,
                    )?
                }
                ScalarExpr::Cast { expr, .. } => expr.copy_for_rewrite(control)?,
                value => value.copy_for_rewrite(control)?,
            })
        } else {
            None
        };
        observe(name, selected)?;
    }
    Ok(())
}

pub(super) fn constant_literal(
    expression: &ScalarExpr,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    Ok(match expression {
        ScalarExpr::Collated { expr, .. } => constant_literal(expr, types, control)?,
        ScalarExpr::Literal(_)
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::TypedLiteral {
            parameter_index: None,
            ..
        } => true,
        ScalarExpr::Cast { expr, ty, .. } => {
            let (original, bound_type) = match expr.as_ref() {
                ScalarExpr::TypedLiteral {
                    ty,
                    bound_type,
                    parameter_index: None,
                    ..
                } => (ty.as_str(), bound_type.as_ref()),
                ScalarExpr::RetainedLiteral(literal) => {
                    let Some(declaration) = literal.declaration.as_deref() else {
                        return Ok(false);
                    };
                    (declaration, literal.ty.as_ref())
                }
                _ => return Ok(false),
            };
            if original == ty {
                return Ok(true);
            }
            match (bound_type, types) {
                (Some(ColumnType::Composite(reference)), Some(types)) => {
                    match types.resolve_catalog_column_type_name_with_control(ty, control) {
                        Ok(target) => {
                            matches!(&*target, ColumnType::Composite(target) if target.oid == reference.oid)
                        }
                        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                            return Err(error)
                        }
                        Err(_) => false,
                    }
                }
                _ => false,
            }
        }
        _ => false,
    })
}

pub(super) fn constant_group(
    block: &QueryBlockPlan,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    if block.group_by.is_empty() || !block.grouping_sets.is_empty() {
        return Ok(false);
    }
    for expression in &block.group_by {
        if !constant_literal(expression, types, control)? {
            return Ok(false);
        }
    }
    let mut grouping = false;
    block.visit_expressions(&mut |expression| {
        expression.visit(&mut |node| {
            if let ScalarExpr::Func { name, .. } = node {
                grouping |= name.eq_ignore_ascii_case("grouping");
            }
        });
    });
    control.check()?;
    Ok(!grouping)
}
