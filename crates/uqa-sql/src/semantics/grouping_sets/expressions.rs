//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Use one executable tree for expressions with the same analyzed grouping identity.

use super::expression_identity;
use crate::plan::QueryBlockPlan;
use crate::semantics::{aggregates::exprs_match, projection_label_at};
use crate::{FunctionTypeResolver, RowSchema, SQLError, SQLParam, ScalarExpr};

pub(super) fn bind_grouping_expressions(
    resolver: &dyn FunctionTypeResolver,
    statement: &mut QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    // Resolve local bare names before structural aggregate matching: an enclosing qualified column with the same spelling is a different input.
    let mut changed = false;
    for expression in statement
        .group_by
        .iter_mut()
        .chain(statement.grouping_sets.iter_mut().flatten())
    {
        changed |= qualify_columns(expression, schema);
    }
    for projection in &mut statement.projections {
        let label = projection_label_at(projection);
        if qualify_columns(&mut projection.expr, schema) {
            if projection.alias.is_none() && projection_label_at(projection) != label {
                projection.alias = Some(label);
            }
            changed = true;
        }
    }
    if let Some(having) = &mut statement.having {
        changed |= qualify_columns(having, schema);
    }
    let groups = statement
        .group_by
        .iter()
        .chain(statement.grouping_sets.iter().flatten())
        .map(|expression| {
            Ok((
                expression_identity(resolver, expression, schema, params)?,
                expression,
            ))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let rewrite = |expression: &mut ScalarExpr| {
        let mut result = Ok(false);
        crate::plan::rewrite_scalar_expression(expression, &mut |part| {
            let Ok(changed) = &mut result else {
                return;
            };
            match expression_identity(resolver, part, schema, params) {
                Ok(identity) => {
                    if let Some((_, group)) = groups.iter().find(|(key, _)| *key == identity) {
                        if !exprs_match(part, group) {
                            *part = (*group).clone();
                            *changed = true;
                        }
                    }
                }
                Err(error) => result = Err(error),
            }
        });
        result
    };
    for projection in &mut statement.projections {
        let label = projection_label_at(projection);
        if rewrite(&mut projection.expr)? {
            if projection.alias.is_none() && projection_label_at(projection) != label {
                projection.alias = Some(label);
            }
            changed = true;
        }
    }
    for expression in statement
        .having
        .iter_mut()
        .chain(statement.order_by.iter_mut().map(|order| &mut order.expr))
        .chain(statement.distinct_on.iter_mut())
    {
        changed |= rewrite(expression)?;
    }
    Ok(changed)
}

fn qualify_columns(expression: &mut ScalarExpr, schema: &RowSchema) -> bool {
    let mut changed = false;
    expression.visit_mut(&mut |part| {
        let ScalarExpr::Column(name) = part else {
            return;
        };
        let Some(identity) = schema
            .unqualified_position(name)
            .and_then(|position| schema.identity(position))
        else {
            return;
        };
        if let Some(qualifier) = identity.qualifier() {
            *part = ScalarExpr::QualifiedColumn {
                qualifier: qualifier.to_owned(),
                column: identity.column().into(),
            };
            changed = true;
        }
    });
    changed
}
