//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Store typed ROW constructors with attribute positions chosen by their original analysis.

use super::FunctionTypeResolver;
use crate::ast::{ColumnType, CompositeRowBinding};
use crate::schema::ScalarTypeSchema;
use crate::{SQLError, SQLParam, ScalarExpr};

/// Detect predecessor row constructors and unknown input literals before a descriptor can acquire additional attributes. Ordinary casts to strings and domains over non-composite types do not need conversion.
pub fn expression_requires_binding(
    expression: &crate::ast::Expr,
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, SQLError> {
    let mut expression = expression.clone();
    let mut found = false;
    crate::catalog::stored_ast::visit_stored_expression(&mut expression, &mut |node| {
        if let crate::ast::Expr::Cast { expr, ty, .. } = node {
            if !found
                && matches!(
                    expr.as_ref(),
                    crate::ast::Expr::Row(_) | crate::ast::Expr::Literal(uqa_core::Value::Str(_))
                )
            {
                found = resolver
                    .resolve_type_name(ty)?
                    .is_some_and(|ty| crate::expr::requires_catalog_constant_input(&ty));
            }
        }
        Ok(())
    })?;
    Ok(found)
}

pub fn statement_requires_binding(
    statement: &mut crate::ast::Statement,
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, SQLError> {
    let mut found = false;
    crate::catalog::stored_ast::visit_stored_statement_expressions(statement, &mut |node| {
        if let crate::ast::Expr::Cast { expr, ty, .. } = node {
            if !found
                && matches!(
                    expr.as_ref(),
                    crate::ast::Expr::Row(_) | crate::ast::Expr::Literal(uqa_core::Value::Str(_))
                )
            {
                found = resolver
                    .resolve_type_name(ty)?
                    .is_some_and(|ty| crate::expr::requires_catalog_constant_input(&ty));
            }
        }
        Ok(())
    })?;
    Ok(found)
}

pub fn query_requires_binding(
    query: &mut crate::plan::QueryPlan,
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, SQLError> {
    let mut found = false;
    let mut failure = None;
    query.rewrite_scalar_expressions(&mut |node| inspect(node, resolver, &mut found, &mut failure));
    failure.map_or(Ok(found), Err)
}

fn inspect(
    node: &ScalarExpr,
    resolver: &dyn FunctionTypeResolver,
    found: &mut bool,
    failure: &mut Option<SQLError>,
) {
    if *found || failure.is_some() {
        return;
    }
    if let ScalarExpr::Cast { expr, ty, .. } = node {
        if matches!(
            expr.as_ref(),
            ScalarExpr::Row(_) | ScalarExpr::Literal(uqa_core::Value::Str(_))
        ) {
            match resolver.resolve_type_name(ty) {
                Ok(Some(ty)) => {
                    *found = crate::expr::requires_catalog_constant_input(&ty);
                }
                Ok(None) => {}
                Err(error) => *failure = Some(error),
            }
        }
    }
}

pub(crate) fn bind_stored_row(
    expression: &mut ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, SQLError> {
    if let ScalarExpr::CompositeRow { binding, .. } = expression {
        return crate::expr::composites::constructor::retain_argument_types(
            binding,
            resolver,
            resolver.composite_types(),
        );
    }
    let ScalarExpr::Cast { expr, ty, .. } = expression else {
        return Ok(false);
    };
    let ScalarExpr::Row(items) = expr.as_ref() else {
        return Ok(false);
    };
    let Some(target) = resolver.resolve_type_name(ty)? else {
        return Ok(false);
    };
    let ColumnType::Composite(reference) = super::common::base_type(&target) else {
        return Ok(false);
    };
    let reference = reference.clone();
    let domain = matches!(target, ColumnType::Domain { .. });
    let descriptor =
        crate::expr::composites::descriptor(resolver.composite_types(), reference.oid)?;
    validate_width(
        &ColumnType::Composite(reference.clone()),
        items.len(),
        descriptor.attributes.len(),
    )?;
    let mut converted = Vec::with_capacity(items.len());
    for (item, attribute) in items.iter().zip(&descriptor.attributes) {
        let source = super::scalar_type_with_resolver(item, schema, params, resolver)?;
        if source
            .as_ref()
            .is_some_and(|source| !super::explicit_type_compatible(source, &attribute.ty))
        {
            return Err(SQLError::Routine {
                sqlstate: "42846".into(),
                message: format!(
                    "cannot cast type {} to {}",
                    source.as_ref().unwrap().display_name(),
                    attribute.ty.display_name()
                ),
            });
        }
        let converted_item = if source.as_ref() == Some(&attribute.ty) {
            item.clone()
        } else {
            ScalarExpr::Cast {
                implicit: true,
                expr: Box::new(item.clone()),
                ty: attribute
                    .ty
                    .user_type_identity()
                    .unwrap_or_else(|| attribute.ty.catalog_name()),
            }
        };
        converted.push(super::introspection::bind_stored_inputs(
            converted_item,
            schema,
            params,
            resolver,
        )?);
    }
    let row = ScalarExpr::CompositeRow {
        items: converted,
        bound_type: Some(ColumnType::Composite(reference.clone())),
        binding: CompositeRowBinding {
            ty: if domain {
                ColumnType::Composite(reference)
                    .user_type_identity()
                    .expect("composite identity")
            } else {
                ty.clone()
            },
            attributes: descriptor
                .attributes
                .iter()
                .map(|attribute| attribute.number)
                .collect(),
            argument_types: Some(
                descriptor
                    .attributes
                    .iter()
                    .map(|attribute| attribute.ty.clone())
                    .collect(),
            ),
        },
    };
    if domain {
        **expr = row;
    } else {
        *expression = row;
    }
    Ok(true)
}

/// Validate an analyzed ROW cast before any surrounding field lookup.
pub(crate) fn validate_width(
    target: &ColumnType,
    width: usize,
    expected: usize,
) -> Result<(), SQLError> {
    if width != expected {
        return Err(SQLError::Diagnostic {
            sqlstate: "42846".into(),
            message: format!(
                "cannot cast type record to {}",
                target.clone().display_name()
            ),
            detail: Some(
                if width < expected {
                    "Input has too few columns."
                } else {
                    "Input has too many columns."
                }
                .into(),
            ),
            hint: None,
        });
    }
    Ok(())
}
