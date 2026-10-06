//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read OID alias constants at analysis as the `reg*` input functions read them: a `regclass`, `regtype`, `regproc`, `regprocedure` or `regnamespace` cast of a string literal, or a cast to an array of one of them, stores the OIDs the names resolve to, as parse analysis stores the constant, so a stored expression follows the objects through renames and a name no object has is reported where the expression is analyzed.

use super::walk_schema_expr_mut;
use crate::ast::{ColumnType, Expr};
use crate::expr::EngineHook;
use crate::plan::{QueryPlan, UnifiedPlan};
use crate::{SQLError, ScalarExpr};
use uqa_core::{ArrayValue, Value};

/// A catalog that reads OID alias input: the object a name denotes for one alias type, resolved as the type's input function resolves it through the search path, with the input function's errors. `None` says the catalog has no such object; the reader then reports the input function's missing-object error.
pub trait OidAliasInput {
    fn resolve_oid_alias_input(&self, ty: &ColumnType, name: &str)
        -> Result<Option<i64>, SQLError>;
}

impl<T: EngineHook + ?Sized> OidAliasInput for T {
    fn resolve_oid_alias_input(
        &self,
        ty: &ColumnType,
        name: &str,
    ) -> Result<Option<i64>, SQLError> {
        match ty {
            ColumnType::Regclass => EngineHook::resolve_regclass_input(self, name),
            ColumnType::Regtype => EngineHook::resolve_regtype_input(self, name),
            ColumnType::Regproc => EngineHook::resolve_regproc(self, name),
            ColumnType::Regprocedure => EngineHook::resolve_regprocedure_input(self, name),
            ColumnType::Regnamespace => EngineHook::resolve_regnamespace(self, name),
            ColumnType::Regrole => EngineHook::resolve_regrole(self, name),
            other => Err(SQLError::Internal(format!(
                "{} is not an OID alias type read at analysis",
                other.sql_name()
            ))),
        }
    }
}

/// Whether `ty` is an OID alias type whose input function reads a catalog name at analysis.
fn is_alias(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Regclass
            | ColumnType::Regtype
            | ColumnType::Regproc
            | ColumnType::Regprocedure
            | ColumnType::Regnamespace
    )
}

/// The OID alias type a cast's type name denotes, and whether the cast is to an array of it.
fn alias_type(ty: &str) -> Option<(ColumnType, bool)> {
    if !ty
        .as_bytes()
        .windows(3)
        .any(|window| window.eq_ignore_ascii_case(b"reg"))
    {
        return None;
    }
    match ColumnType::from_sql_name(ty).ok()? {
        ColumnType::Array(element) if is_alias(&element) => Some((*element, true)),
        element if is_alias(&element) => Some((element, false)),
        _ => None,
    }
}

/// The error the alias type's input function reports for a name no object has.
fn missing_object(ty: &ColumnType, name: &str) -> SQLError {
    let (sqlstate, object) = match ty {
        ColumnType::Regclass => ("42P01", "relation"),
        ColumnType::Regtype => ("42704", "type"),
        ColumnType::Regrole => ("42704", "role"),
        ColumnType::Regproc | ColumnType::Regprocedure => ("42883", "function"),
        _ => ("3F000", "schema"),
    };
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: format!("{object} \"{name}\" does not exist"),
    }
}

/// The OID the alias type's input function gives `name`.
fn read_name<C: OidAliasInput + ?Sized>(
    catalog: &C,
    ty: &ColumnType,
    name: &str,
) -> Result<i64, SQLError> {
    catalog
        .resolve_oid_alias_input(ty, name)?
        .ok_or_else(|| missing_object(ty, name))
}

/// The constant parse analysis stores for a cast of the literal `text` to the alias type `ty`, or to an array of it: the object's OID, or the OIDs of the array's elements.
fn read_constant<C: OidAliasInput + ?Sized>(
    catalog: &C,
    ty: &ColumnType,
    text: &str,
    array: bool,
) -> Result<Value, SQLError> {
    if !array {
        return read_name(catalog, ty, text).map(Value::Int);
    }
    let array = crate::expr::parse_pg_array_literal(text)?;
    let lower_bounds = array.lower_bounds().to_vec();
    let mut elements = array.into_elements();
    read_array_elements(catalog, ty, &mut elements)?;
    ArrayValue::with_lower_bounds(elements, lower_bounds)
        .map(Value::Array)
        .ok_or_else(|| {
            SQLError::Internal(format!("{} array literal lost its shape", ty.sql_name()))
        })
}

fn read_array_elements<C: OidAliasInput + ?Sized>(
    catalog: &C,
    ty: &ColumnType,
    elements: &mut [Value],
) -> Result<(), SQLError> {
    for element in elements {
        match element {
            Value::Null => {}
            Value::Str(name) => *element = Value::Int(read_name(catalog, ty, name)?),
            Value::List(nested) => read_array_elements(catalog, ty, nested)?,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "cannot read {other:?} as {}",
                    ty.sql_name(),
                )))
            }
        }
    }
    Ok(())
}

/// Read an unknown input after ordered analysis selects an OID alias type. Other catalog-dependent input types remain the responsibility of their own binders.
pub(crate) fn read_unknown_constant(
    catalog: &dyn OidAliasInput,
    ty: &ColumnType,
    text: &str,
) -> Result<Option<Value>, SQLError> {
    match ty {
        ColumnType::Array(element)
            if is_alias(element) || matches!(element.as_ref(), ColumnType::Regrole) =>
        {
            read_constant(catalog, element, text, true).map(Some)
        }
        scalar if is_alias(scalar) || matches!(scalar, ColumnType::Regrole) => {
            read_constant(catalog, scalar, text, false).map(Some)
        }
        _ => Ok(None),
    }
}

/// The type of the stored constant: the alias type, or an array of it.
fn constant_type(ty: ColumnType, array: bool) -> ColumnType {
    if array {
        ColumnType::Array(Box::new(ty))
    } else {
        ty
    }
}

/// Read the OID alias constants of a stored schema expression: each `reg*` cast of a string literal becomes the typed constant of the OIDs the names resolve to, so the expression follows the objects through renames, and a name no object has reports the input function's error.
pub fn read_oid_alias_constants<C: OidAliasInput + ?Sized>(
    catalog: &C,
    expression: &mut Expr,
) -> Result<(), SQLError> {
    let mut failure = None;
    let outcome = walk_schema_expr_mut(expression, &mut |node| {
        let Expr::Cast { expr, ty } = node else {
            return Ok(());
        };
        let Some((alias, array)) = alias_type(ty) else {
            return Ok(());
        };
        let Expr::Literal(Value::Str(text)) = expr.as_ref() else {
            return Ok(());
        };
        match read_constant(catalog, &alias, text, array) {
            Ok(value) => {
                **expr = Expr::TypedLiteral {
                    value,
                    ty: constant_type(alias, array).catalog_name(),
                };
                Ok(())
            }
            Err(error) => {
                failure = Some(error);
                Err(String::new())
            }
        }
    });
    match (outcome, failure) {
        (Ok(()), _) => Ok(()),
        (Err(_), Some(error)) => Err(error),
        (Err(message), None) => Err(SQLError::Internal(message)),
    }
}

/// The first argument of `nextval`, `currval` or `setval` when it is an `unknown` literal: `coerce_type` reads it with `regclassin` for the `regclass` parameter, so the stored constant is the sequence's OID and prints as `'name'::regclass`.
fn sequence_argument_mut(expression: &mut ScalarExpr) -> Option<&mut ScalarExpr> {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return None;
    };
    if !is_sequence_function(name) {
        return None;
    }
    args.first_mut()
        .filter(|argument| matches!(argument, ScalarExpr::Literal(Value::Str(_))))
}

fn sequence_argument(expression: &ScalarExpr) -> Option<&str> {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return None;
    };
    if !is_sequence_function(name) {
        return None;
    }
    match args.first() {
        Some(ScalarExpr::Literal(Value::Str(text))) => Some(text),
        _ => None,
    }
}

fn is_sequence_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let local = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    matches!(local, "nextval" | "currval" | "setval")
        && (!lower.contains('.') || lower.starts_with("pg_catalog."))
}

/// [`read_oid_alias_constants`] for one scalar node. The first failure is kept and stops further reads. With `keep_relations`, a `regclass` cast is checked and left as written.
fn read_scalar_constant<C: OidAliasInput + ?Sized>(
    catalog: &C,
    expression: &mut ScalarExpr,
    keep_relations: bool,
    failure: &mut Option<SQLError>,
) {
    if failure.is_some() {
        return;
    }
    if let Some(argument) = sequence_argument_mut(expression) {
        let ScalarExpr::Literal(Value::Str(text)) = &*argument else {
            unreachable!("sequence argument is an unknown literal");
        };
        match read_constant(catalog, &ColumnType::Regclass, text, false) {
            Ok(value) => {
                if !keep_relations {
                    *argument = ScalarExpr::TypedLiteral {
                        value,
                        ty: ColumnType::Regclass.catalog_name(),
                        bound_type: Some(ColumnType::Regclass),
                        parameter_index: None,
                    };
                }
            }
            Err(error) => *failure = Some(error),
        }
        return;
    }
    let ScalarExpr::Cast { expr, ty, .. } = expression else {
        return;
    };
    let Some((alias, array)) = alias_type(ty) else {
        return;
    };
    let ScalarExpr::Literal(Value::Str(text)) = expr.as_ref() else {
        return;
    };
    match read_constant(catalog, &alias, text, array) {
        Ok(value) => {
            if keep_relations && matches!(alias, ColumnType::Regclass) {
                return;
            }
            let bound_type = constant_type(alias, array);
            **expr = ScalarExpr::TypedLiteral {
                value,
                ty: bound_type.catalog_name(),
                bound_type: Some(bound_type),
                parameter_index: None,
            };
        }
        Err(error) => *failure = Some(error),
    }
}

/// [`read_oid_alias_constants`] over every scalar expression of a stored query, nested ones included: a view stores the OIDs its `reg*` constants name.
pub fn read_oid_alias_constants_in_plan<C: OidAliasInput + ?Sized>(
    catalog: &C,
    plan: &mut QueryPlan,
) -> Result<(), SQLError> {
    let mut failure = None;
    plan.rewrite_scalar_expressions(&mut |root| {
        root.visit_mut(&mut |expression| {
            read_scalar_constant(catalog, expression, false, &mut failure);
        });
    });
    failure.map_or(Ok(()), Err)
}

/// Read a prepared statement's OID alias inputs once. Scalar regclass constants contribute relation dependencies separately; invalidation reads the retained original syntax again. A whole array constant retains its OIDs without becoming scalar relation dependencies.
pub fn read_prepared_oid_alias_constants<C: OidAliasInput + ?Sized>(
    catalog: &C,
    plan: &mut UnifiedPlan,
) -> Result<(), SQLError> {
    let mut failure = None;
    plan.rewrite_scalar_expressions(&mut |root| {
        root.visit_mut(&mut |expression| {
            read_scalar_constant(catalog, expression, false, &mut failure);
        });
    });
    failure.map_or(Ok(()), Err)
}

/// The alias input functions at analysis for a statement that runs at once: every `reg*` cast of a string literal, nested ones included, must name an object before the statement runs, as parse analysis reads the constant before planning and execution. The plan keeps the written names, which the cast resolves again when it is evaluated, since a statement's text is analyzed anew each time it runs.
pub fn check_statement_oid_alias_constants<C: OidAliasInput + ?Sized>(
    catalog: &C,
    plan: &UnifiedPlan,
) -> Result<(), SQLError> {
    let mut failure = None;
    plan.visit_scalar_expressions(&mut |root| {
        root.visit(&mut |expression| {
            if failure.is_some() {
                return;
            }
            if let Some(text) = sequence_argument(expression) {
                if let Err(error) = read_constant(catalog, &ColumnType::Regclass, text, false) {
                    failure = Some(error);
                }
                return;
            }
            let ScalarExpr::Cast { expr, ty, .. } = expression else {
                return;
            };
            let Some((alias, array)) = alias_type(ty) else {
                return;
            };
            let ScalarExpr::Literal(Value::Str(text)) = expr.as_ref() else {
                return;
            };
            if let Err(error) = read_constant(catalog, &alias, text, array) {
                failure = Some(error);
            }
        });
    });
    failure.map_or(Ok(()), Err)
}
