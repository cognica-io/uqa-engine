//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 restricts virtual generated columns to built-in functions and types (`check_virtual_generated_security`), because reading such a column evaluates its expression with the reader's privileges. The walk is pre-order: a node's own function is checked, then its result type, then its children.

use crate::ast::{ColumnDef, ColumnType, Expr};
use crate::schema::SchemaExpressionCatalog;
use crate::SQLError;

pub(super) const USER_DEFINED_TYPE_DETAIL: &str =
    "Virtual generated columns that make use of user-defined types are not yet supported.";
const USER_DEFINED_FUNCTION_DETAIL: &str =
    "Virtual generated columns that make use of user-defined functions are not yet supported.";

/// Whether a declared type is a catalog object created by a user: an enum, a domain, or an array of either.
pub(super) fn is_user_defined_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Enum(_) | ColumnType::Composite(_) | ColumnType::Domain { .. } => true,
        ColumnType::Array(element) => is_user_defined_type(element),
        _ => false,
    }
}

/// Host callbacks carry no declared SQL types, so static typing cannot analyze them; decide their outcome in `PostgreSQL`'s order first: a callback that is not immutable fails the immutability check, and an immutable one is a user-defined function.
pub(super) fn check_virtual_host_functions(
    engine: &dyn SchemaExpressionCatalog,
    expression: &Expr,
) -> Result<(), SQLError> {
    let mut host_function = None;
    visit(expression, &mut |node| {
        if let Expr::Func {
            name,
            binding: None,
            ..
        } = node
        {
            if let Some(volatility) = engine.registered_runtime_function_volatility(name) {
                let slot = host_function.get_or_insert(volatility);
                if volatility != crate::ast::FunctionVolatility::Immutable {
                    *slot = volatility;
                }
            }
        }
    });
    match host_function {
        None => Ok(()),
        Some(crate::ast::FunctionVolatility::Immutable) => Err(user_defined_function()),
        Some(_) => Err(super::eligibility::non_immutable_function()),
    }
}

fn visit<'a>(expression: &'a Expr, visitor: &mut dyn FnMut(&'a Expr)) {
    visitor(expression);
    for child in children(expression) {
        visit(child, visitor);
    }
}

fn user_defined_function() -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "0A000".into(),
        message: "generation expression uses user-defined function".into(),
        detail: Some(USER_DEFINED_FUNCTION_DETAIL.into()),
        hint: None,
    }
}

pub(super) fn check_virtual_generated_security(
    engine: &dyn SchemaExpressionCatalog,
    columns: &[ColumnDef],
    expression: &Expr,
) -> Result<(), SQLError> {
    if let Expr::Func { name, binding, .. } = expression {
        let user_function = binding.as_ref().map_or_else(
            || {
                engine
                    .registered_runtime_function_volatility(name)
                    .is_some()
            },
            |binding| !binding.builtin,
        );
        if user_function {
            return Err(user_defined_function());
        }
    }
    if node_has_user_defined_type(engine, columns, expression)? {
        return Err(SQLError::Diagnostic {
            sqlstate: "0A000".into(),
            message: "generation expression uses user-defined type".into(),
            detail: Some(USER_DEFINED_TYPE_DETAIL.into()),
            hint: None,
        });
    }
    for child in children(expression) {
        check_virtual_generated_security(engine, columns, child)?;
    }
    Ok(())
}

fn node_has_user_defined_type(
    engine: &dyn SchemaExpressionCatalog,
    columns: &[ColumnDef],
    expression: &Expr,
) -> Result<bool, SQLError> {
    if let Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } = expression {
        return Ok(columns
            .iter()
            .find(|column| column.name == *name)
            .is_some_and(|column| is_user_defined_type(&column.ty)));
    }
    let scalar = crate::plan::ExpressionPlan::lower(expression.clone()).scalar;
    Ok(crate::scalar_type_with_resolver(
        &scalar,
        &super::super::expressions::row_schema(columns),
        &[],
        engine,
    )?
    .as_ref()
    .is_some_and(is_user_defined_type))
}

fn children(expression: &Expr) -> Vec<&Expr> {
    match expression {
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => args
            .iter()
            .chain(order_by.iter().map(|order| &order.expr))
            .chain(filter.as_deref())
            .collect(),
        Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
            items.iter().collect()
        }
        Expr::Binary { lhs, rhs, .. } => vec![lhs, rhs],
        Expr::Not(inner)
        | Expr::UnaryMinus(inner)
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => vec![inner],
        Expr::Between { expr, low, high } => vec![expr, low, high],
        Expr::InList { expr, list, .. } => std::iter::once(expr.as_ref()).chain(list).collect(),
        Expr::Case {
            base,
            when,
            else_branch,
        } => base
            .as_deref()
            .into_iter()
            .chain(
                when.iter()
                    .flat_map(|(condition, result)| [condition, result]),
            )
            .chain(else_branch.as_deref())
            .collect(),
        Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Default
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::TypedLiteral { .. }
        | Expr::Param(_)
        | Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => Vec::new(),
    }
}
