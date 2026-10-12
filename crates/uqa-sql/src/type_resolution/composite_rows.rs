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
        if !found {
            found = syntax_requires_binding(node, resolver)?;
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
        if !found {
            found = syntax_requires_binding(node, resolver)?;
        }
        Ok(())
    })?;
    Ok(found)
}

fn syntax_requires_binding(
    expression: &crate::ast::Expr,
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, SQLError> {
    let crate::ast::Expr::Cast { expr, ty, .. } = expression else {
        return Ok(false);
    };
    if !matches!(
        expr.as_ref(),
        crate::ast::Expr::Literal(uqa_core::Value::Str(_))
    ) && !anonymous_row_syntax(expr, resolver)?
    {
        return Ok(false);
    }
    Ok(resolver
        .resolve_type_name(ty)?
        .is_some_and(|ty| crate::expr::requires_catalog_constant_input(&ty)))
}

fn anonymous_row_syntax(
    expression: &crate::ast::Expr,
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, SQLError> {
    match expression {
        crate::ast::Expr::Row(_) => Ok(true),
        crate::ast::Expr::Cast { expr, ty, .. }
            if resolver.resolve_type_name(ty)? == Some(ColumnType::Record) =>
        {
            anonymous_row_syntax(expr, resolver)
        }
        _ => Ok(false),
    }
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
    let ScalarExpr::Cast { expr, ty, .. } = node else {
        return;
    };
    let required = (|| {
        if matches!(expr.as_ref(), ScalarExpr::Literal(uqa_core::Value::Str(_)))
            || anonymous_row_elements(expr, resolver)?.is_some()
        {
            Ok(resolver
                .resolve_type_name(ty)?
                .is_some_and(|ty| crate::expr::requires_catalog_constant_input(&ty)))
        } else {
            Ok(false)
        }
    })();
    match required {
        Ok(required) => *found = required,
        Err(error) => *failure = Some(error),
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
    let ScalarExpr::Cast { expr, ty, implicit } = expression else {
        return Ok(false);
    };
    let Some(items) = anonymous_row_elements(expr, resolver)? else {
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
    let mut converted = Vec::with_capacity(items.len());
    for (index, attribute) in descriptor.attributes.iter().enumerate() {
        let Some(item) = items.get(index) else {
            return Err(row_cast_error(
                &target,
                "Input has too few columns.".into(),
                Some(resolver),
            ));
        };
        let source = super::scalar_type_with_resolver(item, schema, params, resolver)?;
        validate_field_cast(
            &target,
            source.as_ref(),
            &attribute.ty,
            index + 1,
            Some(resolver),
        )?;
        super::common::read_unknown_literals(std::iter::once(item), &attribute.ty)?;
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
        let mut converted_item =
            super::introspection::bind_stored_inputs(converted_item, schema, params, resolver)?;
        bind_stored_row(&mut converted_item, schema, params, resolver)?;
        converted.push(converted_item);
    }
    validate_width(
        &target,
        items.len(),
        descriptor.attributes.len(),
        Some(resolver),
    )?;
    let row = ScalarExpr::CompositeRow {
        items: converted,
        bound_type: Some(ColumnType::Composite(reference.clone())),
        binding: CompositeRowBinding {
            layout: Some(crate::ast::CompositeRowLayout {
                width: descriptor.physical_width(),
                implicit: domain || *implicit,
            }),
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

fn anonymous_row_elements<'a>(
    expression: &'a ScalarExpr,
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<&'a [ScalarExpr]>, SQLError> {
    match expression {
        ScalarExpr::Row(items) => Ok(Some(items)),
        ScalarExpr::Cast { expr, ty, .. }
            if resolver.resolve_type_name(ty)? == Some(ColumnType::Record) =>
        {
            anonymous_row_elements(expr, resolver)
        }
        _ => Ok(None),
    }
}

/// Validate an analyzed ROW cast before any surrounding field lookup.
pub(crate) fn validate_width(
    target: &ColumnType,
    width: usize,
    expected: usize,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<(), SQLError> {
    if width != expected {
        return Err(row_cast_error(
            target,
            if width < expected {
                "Input has too few columns."
            } else {
                "Input has too many columns."
            }
            .into(),
            resolver,
        ));
    }
    Ok(())
}

pub(crate) fn validate_field_cast(
    row_type: &ColumnType,
    source: Option<&ColumnType>,
    target: &ColumnType,
    position: usize,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<(), SQLError> {
    if let Some(source) = source {
        if !super::explicit_type_compatible(source, target) {
            return Err(row_cast_error(
                row_type,
                format!(
                    "Cannot cast type {} to {} in column {position}.",
                    format_type(source, resolver)?,
                    format_type(target, resolver)?
                ),
                resolver,
            ));
        }
    }
    Ok(())
}

fn format_type(
    ty: &ColumnType,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<String, SQLError> {
    resolver.map_or_else(
        || Ok(ty.without_type_modifiers().sql_name()),
        |resolver| resolver.format_type_name(&ty.without_type_modifiers()),
    )
}

pub(crate) fn row_cast_error(
    target: &ColumnType,
    detail: String,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> SQLError {
    let target = match format_type(target, resolver) {
        Ok(target) => target,
        Err(error) => return error,
    };
    SQLError::Diagnostic {
        sqlstate: "42846".into(),
        message: format!("cannot cast type record to {target}"),
        detail: Some(detail),
        hint: None,
    }
}
