//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column default validation and binding, as `cookDefault` analyzes a default: the expression is checked for what a default cannot contain, reduced to a constant when it is one, and coerced to the column's type in assignment context.

use crate::ast::{ColumnType, Expr};
use crate::{RowSchema, SQLError};
use uqa_core::Value;

use super::SchemaBindingContext;

/// Analyze a default as `cookDefault` does and bind it for storage. `column` names the column, or the domain, the default belongs to. Returns whether the default remains: a default that cooks to a NULL constant is dropped, as `AddRelationNewConstraints` and `DefineDomain` store no default for it.
pub fn validate_default_expression(
    context: &SchemaBindingContext<'_, '_>,
    expression: &mut Expr,
    target: &ColumnType,
    column: &str,
) -> Result<bool, SQLError> {
    let plan = crate::plan::ExpressionPlan::lower(expression.clone());
    if !plan.subqueries.is_empty() {
        return Err(default_error(
            "0A000",
            "cannot use subquery in DEFAULT expression",
        ));
    }
    if crate::semantics::windows::expr_has_window(&plan.scalar) {
        return Err(default_error(
            "42P20",
            "window functions are not allowed in DEFAULT expressions",
        ));
    }
    if crate::semantics::aggregates::contains_aggregate(context.catalog, &plan.scalar) {
        return Err(default_error(
            "42803",
            "aggregate functions are not allowed in DEFAULT expressions",
        ));
    }
    if crate::semantics::aggregates::expr_references_columns(&plan.scalar) {
        return Err(default_error(
            "0A000",
            "cannot use column reference in DEFAULT expression",
        ));
    }
    if crate::semantics::sets::validation::expression_may_return_set(
        context.catalog,
        context.catalog,
        &plan.scalar,
        &RowSchema::default(),
        &[],
    )? {
        return Err(default_error(
            "0A000",
            "set-returning functions are not allowed in DEFAULT expressions",
        ));
    }
    crate::catalog::regrole_dependencies::reject_stored_regrole_constants(
        context.catalog,
        expression,
        Some(target),
    )?;
    if !cook_constant(context, expression, target)? {
        return Ok(false);
    }
    // The cooked expression has the type `coerce_to_target_type` checks: a literal the column type read is of that type.
    let cooked = crate::plan::ExpressionPlan::lower(expression.clone());
    let source = crate::type_resolution::scalar_type_with_resolver(
        &cooked.scalar,
        &RowSchema::default(),
        &[],
        context.catalog,
    )?;
    if let Some(source) = source {
        check_assignable(&source, target, column, "default expression")?;
    }
    bind_stored_schema_expression(context, expression, expression.clone())?;
    Ok(true)
}

/// `coerce_to_target_type` in assignment context: the expression's type must have an assignment cast to the column's type.
pub fn check_assignable(
    source: &ColumnType,
    target: &ColumnType,
    column: &str,
    expression: &str,
) -> Result<(), SQLError> {
    if crate::type_resolution::assignment_type_compatible(source, target) {
        return Ok(());
    }
    Err(SQLError::Diagnostic {
        sqlstate: "42804".into(),
        message: format!(
            "column \"{column}\" is of type {} but {expression} is of type {}",
            target.regtype_name(),
            source.regtype_name()
        ),
        detail: None,
        hint: Some("You will need to rewrite or cast the expression.".into()),
    })
}

/// Reduce a default that is a constant as parse analysis reduces it: an `unknown` literal is read by the column type's input function and stored as a constant of that type, a cast of a literal is read by the cast's type, and a NULL constant leaves no default. Returns whether a default remains.
pub fn cook_constant(
    context: &SchemaBindingContext<'_, '_>,
    expression: &mut Expr,
    target: &ColumnType,
) -> Result<bool, SQLError> {
    match expression {
        Expr::Literal(Value::Null) => return Ok(false),
        Expr::TypedLiteral {
            value: Value::Null, ..
        } => return Ok(false),
        Expr::Cast { expr, .. } if matches!(expr.as_ref(), Expr::Literal(Value::Null)) => {
            return Ok(false);
        }
        Expr::Literal(Value::Str(_)) => {
            cook_unknown_literal(context, expression, target, false)?;
        }
        Expr::Cast { expr, ty } if matches!(expr.as_ref(), Expr::Literal(Value::Str(_))) => {
            // A cast of a literal is read by the cast's type and keeps the modifier the cast writes.
            let cast_type = crate::expr::EngineHook::resolve_type_name(context.catalog, ty)
                .map_err(SQLError::Internal)?
                .map_or_else(|| ColumnType::from_sql_name(ty), Ok)?;
            if crate::type_resolution::catalog_input_type(&cast_type) {
                // The cast is already the form the catalog binding of a stored expression resolves.
                return Ok(true);
            }
            cook_unknown_literal(context, expr, &cast_type, true)?;
            let cooked = std::mem::replace(expr.as_mut(), Expr::Literal(Value::Null));
            *expression = cooked;
        }
        _ => {}
    }
    Ok(true)
}

/// Read an `unknown` literal with the input function of `target`, which reports what the type's input rejects, and store the typed constant. `coerce_type` passes the input function no type modifier, so a length or precision the column declares applies when a row is assigned, not here; the constant keeps the modifier only when `keep_modifier` says a cast wrote it.
pub(super) fn cook_unknown_literal(
    context: &SchemaBindingContext<'_, '_>,
    expression: &mut Expr,
    target: &ColumnType,
    keep_modifier: bool,
) -> Result<(), SQLError> {
    if crate::catalog::stored_ast::fold_assigned_stored_literal(
        expression,
        target,
        crate::FunctionTypeResolver::enum_labels(context.catalog),
    )? {
        return Ok(());
    }
    let Expr::Literal(value) = &*expression else {
        return Ok(());
    };
    let mut base = target;
    while let ColumnType::Domain { base: inner, .. } = base {
        base = inner;
    }
    if crate::type_resolution::catalog_input_type(base) {
        // The input function of an OID alias type resolves the name in the catalog, which the binding of the stored expression does; the literal takes the cast that binding resolves.
        let literal = std::mem::replace(expression, Expr::Literal(Value::Null));
        *expression = Expr::Cast {
            expr: Box::new(literal),
            ty: base.catalog_name(),
        };
        return Ok(());
    }
    let input_type = base.without_type_modifiers();
    let value =
        crate::assignment::conversion::convert_value_to_column_type(value.clone(), &input_type)?;
    let ty = if keep_modifier { base } else { &input_type };
    *expression = Expr::TypedLiteral {
        value,
        ty: ty.catalog_name(),
    };
    Ok(())
}

/// Bind a copy of stored schema expression syntax and carry its exact routine identities, user-defined type identities and enum constants back into the syntax. `typed_expression` is the syntax with its column references replaced by typed placeholders.
pub fn bind_stored_schema_expression(
    context: &SchemaBindingContext<'_, '_>,
    expression: &mut Expr,
    typed_expression: Expr,
) -> Result<bool, SQLError> {
    let lowered = crate::plan::ExpressionPlan::lower_with(typed_expression, &|name: &str| {
        context.catalog.has_registered_aggregate_function(name)
    });
    let mut plan = lowered.clone();
    crate::binding::bind_expression_plan_routines_for_storage(
        context.catalog,
        &mut plan,
        &[],
        context.binding,
        &RowSchema::default(),
    )?;
    let sites = crate::binding::syntax_sites::expression_syntax_sites(&lowered, &plan)?;
    crate::catalog::stored_ast::bind_stored_expression_sites(expression, &sites)
}

fn default_error(sqlstate: &str, message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
