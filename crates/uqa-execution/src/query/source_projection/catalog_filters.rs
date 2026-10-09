//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Necessary identifier equalities from already-approved source filters.

use crate::catalog::projection::CatalogRequest;
use uqa_core::Value;
use uqa_sql::{ast::BinaryOp, ColumnType, ScalarExpr};

pub(super) fn add_name_bounds(
    request: &mut CatalogRequest,
    columns: &[String],
    aliases: &[String],
    qualifier: &str,
    expression: &ScalarExpr,
) {
    if let ScalarExpr::And(parts) = expression {
        for part in parts {
            add_name_bounds(request, columns, aliases, qualifier, part);
        }
    }
    let ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs,
        rhs,
    } = expression
    else {
        return;
    };
    for (column, literal) in [(lhs.as_ref(), rhs.as_ref()), (rhs.as_ref(), lhs.as_ref())] {
        let Some((column, value)) = source_column(column, qualifier).zip(string_literal(literal))
        else {
            continue;
        };
        let mut matching = columns.iter().enumerate().filter(|(index, name)| {
            aliases.get(*index).map_or(name.as_str(), String::as_str) == column
        });
        let Some((_, canonical)) = matching.next() else {
            continue;
        };
        if matching.next().is_none()
            && matches!(
                canonical.as_str(),
                "table_schema" | "table_name" | "column_name" | "typname" | "proname"
            )
        {
            request.require_name(canonical.clone(), value.to_string());
        }
    }
}

fn source_column<'a>(expression: &'a ScalarExpr, qualifier: &str) -> Option<&'a str> {
    match expression {
        ScalarExpr::Column(column) => Some(column),
        ScalarExpr::QualifiedColumn {
            qualifier: owner,
            column,
        } if owner == qualifier => Some(column),
        ScalarExpr::Cast {
            expr,
            implicit: true,
            ty,
        } if matches!(
            ty.as_str(),
            "text" | "pg_catalog.text" | "name" | "pg_catalog.name"
        ) =>
        {
            source_column(expr, qualifier)
        }
        _ => None,
    }
}

fn identifier_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Text | ColumnType::Name => true,
        ColumnType::Domain { base, .. } => identifier_type(base),
        _ => false,
    }
}

fn string_literal(expression: &ScalarExpr) -> Option<&str> {
    match expression {
        ScalarExpr::Literal(Value::Str(value)) => Some(value),
        ScalarExpr::TypedLiteral {
            value: Value::Str(value),
            bound_type: Some(ty),
            composite_source: None,
            ..
        } if identifier_type(ty) => Some(value),
        ScalarExpr::TypedLiteral {
            value: Value::Str(value),
            ty,
            bound_type: None,
            composite_source: None,
            ..
        } if matches!(
            ty.as_str(),
            "text" | "pg_catalog.text" | "name" | "pg_catalog.name"
        ) =>
        {
            Some(value)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
