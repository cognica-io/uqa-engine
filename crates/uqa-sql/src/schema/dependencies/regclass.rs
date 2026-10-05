//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind literal relation references in persisted SQL schema expressions, and read them at analysis as `regclassin` reads them.
use super::walk_schema_expr_mut;
use crate::ast::{ColumnDef, ColumnType, Expr, TableCheck};
use crate::expr::EngineHook;
use crate::plan::{QueryPlan, UnifiedPlan};
use crate::{SQLError, ScalarExpr};
use uqa_core::{ArrayValue, Value};

/// A catalog that resolves a relation name for `regclass` input, as `regclassin` resolves it through the search path.
pub trait RegclassInput {
    fn resolve_regclass_input(&self, name: &str) -> Result<Option<i64>, SQLError>;
}

impl<T: EngineHook + ?Sized> RegclassInput for T {
    fn resolve_regclass_input(&self, name: &str) -> Result<Option<i64>, SQLError> {
        EngineHook::resolve_regclass_input(self, name)
    }
}

fn is_regclass_array(ty: &str) -> bool {
    ty.strip_suffix("[]")
        .map(str::trim_end)
        .is_some_and(is_regclass)
        || ty.eq_ignore_ascii_case("_regclass")
        || ty.eq_ignore_ascii_case("pg_catalog._regclass")
}

fn missing_relation(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P01".into(),
        message: format!("relation \"{name}\" does not exist"),
    }
}

/// The OID `regclassin` gives `name`, which must name a relation.
fn read_regclass_name<C: RegclassInput + ?Sized>(catalog: &C, name: &str) -> Result<i64, SQLError> {
    catalog
        .resolve_regclass_input(name)?
        .ok_or_else(|| missing_relation(name))
}

/// The constant parse analysis stores for a `regclass` or `regclass[]` cast of the literal `text`: the relation OID, or the OIDs of the array's elements.
fn read_regclass_constant<C: RegclassInput + ?Sized>(
    catalog: &C,
    text: &str,
    array: bool,
) -> Result<Value, SQLError> {
    if !array {
        return read_regclass_name(catalog, text).map(Value::Int);
    }
    let elements = crate::expr::parse_pg_array_literal(text)?
        .into_elements()
        .into_iter()
        .map(|element| match element {
            Value::Null => Ok(Value::Null),
            Value::Str(name) => read_regclass_name(catalog, &name).map(Value::Int),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot read {other:?} as regclass"
            ))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    ArrayValue::try_new(elements)
        .map(Value::Array)
        .ok_or_else(|| SQLError::Internal("regclass array literal lost its shape".into()))
}

/// `regclassin` at analysis: a `regclass` or `regclass[]` cast of a string literal stores the relation OIDs the names resolve to, as parse analysis stores the constant, so the expression follows the relations through renames; a name no relation has reports `42P01`.
pub fn read_regclass_constants<C: RegclassInput + ?Sized>(
    catalog: &C,
    expression: &mut Expr,
) -> Result<(), SQLError> {
    let mut failure = None;
    let outcome = walk_schema_expr_mut(expression, &mut |node| {
        let Expr::Cast { expr, ty } = node else {
            return Ok(());
        };
        let array = is_regclass_array(ty.as_str());
        if !array && !is_regclass(ty.as_str()) {
            return Ok(());
        }
        let Expr::Literal(Value::Str(text)) = expr.as_ref() else {
            return Ok(());
        };
        match read_regclass_constant(catalog, text, array) {
            Ok(value) => {
                **expr = Expr::TypedLiteral {
                    value,
                    ty: if array {
                        "regclass[]".into()
                    } else {
                        "regclass".into()
                    },
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

/// [`read_regclass_constants`] for one scalar node: a `regclass` or `regclass[]` cast of a string literal becomes the typed constant. The first failure is kept and stops further reads.
fn read_scalar_regclass_constant<C: RegclassInput + ?Sized>(
    catalog: &C,
    expression: &mut ScalarExpr,
    failure: &mut Option<SQLError>,
) {
    if failure.is_some() {
        return;
    }
    let ScalarExpr::Cast { expr, ty } = expression else {
        return;
    };
    let array = is_regclass_array(ty.as_str());
    if !array && !is_regclass(ty.as_str()) {
        return;
    }
    let ScalarExpr::Literal(Value::Str(text)) = expr.as_ref() else {
        return;
    };
    match read_regclass_constant(catalog, text, array) {
        Ok(value) => {
            let bound_type = if array {
                ColumnType::Array(Box::new(ColumnType::Regclass))
            } else {
                ColumnType::Regclass
            };
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

/// [`read_regclass_constants`] over every scalar expression of a stored query, nested ones included.
pub fn read_regclass_constants_in_plan<C: RegclassInput + ?Sized>(
    catalog: &C,
    plan: &mut QueryPlan,
) -> Result<(), SQLError> {
    let mut failure = None;
    plan.rewrite_scalar_expressions(&mut |root| {
        root.visit_mut(&mut |expression| {
            read_scalar_regclass_constant(catalog, expression, &mut failure);
        });
    });
    failure.map_or(Ok(()), Err)
}

/// `regclassin` at analysis for a prepared statement: every `regclass` or `regclass[]` cast of a string literal, nested ones included, must name a relation when the statement is prepared (`42P01` otherwise). The plan keeps the names: `PostgreSQL` records the relations its `regclass` constants name as dependencies of the prepared plan and analyzes the statement's text anew when one is renamed or dropped, so an `EXECUTE` after such a change resolves the written names again and reports `42P01` for a name no relation has.
pub fn check_regclass_constants_in_statement<C: RegclassInput + ?Sized>(
    catalog: &C,
    plan: &mut UnifiedPlan,
) -> Result<(), SQLError> {
    let mut failure = None;
    plan.rewrite_scalar_expressions(&mut |root| {
        root.visit(&mut |expression| {
            if failure.is_some() {
                return;
            }
            let ScalarExpr::Cast { expr, ty } = expression else {
                return;
            };
            let array = is_regclass_array(ty);
            if !array && !is_regclass(ty) {
                return;
            }
            let ScalarExpr::Literal(Value::Str(text)) = expr.as_ref() else {
                return;
            };
            if let Err(error) = read_regclass_constant(catalog, text, array) {
                failure = Some(error);
            }
        });
    });
    failure.map_or(Ok(()), Err)
}

pub trait SchemaReferenceCatalog {
    fn loaded_relation_name(&self, reference: &str) -> Result<Option<String>, String>;
    fn bound_relation_oid(&self, canonical: &str) -> Result<Option<i64>, String>;
    fn visible_relation_oid(&self, reference: &str) -> Result<Option<i64>, String>;
    fn sequence_for_binding(&self, reference: &str) -> Result<String, String>;
}

fn is_regclass(ty: &str) -> bool {
    ty.eq_ignore_ascii_case("regclass") || ty.eq_ignore_ascii_case("pg_catalog.regclass")
}

pub fn bind_schema_regclass_constants(
    catalog: &dyn SchemaReferenceCatalog,
    expression: &mut Expr,
    loaded: bool,
) -> Result<bool, String> {
    let mut changed = false;
    walk_schema_expr_mut(expression, &mut |node| {
        let Expr::Cast { expr, ty } = node else {
            return Ok(());
        };
        if !is_regclass(ty) {
            return Ok(());
        }
        let Expr::Literal(Value::Str(reference)) = expr.as_ref() else {
            return Ok(());
        };
        let oid = if loaded {
            match reference.parse::<u32>() {
                Ok(oid) => Some(i64::from(oid)),
                Err(_) => match catalog.loaded_relation_name(reference)? {
                    Some(canonical) => catalog.bound_relation_oid(&canonical)?,
                    None => None,
                },
            }
        } else {
            catalog.visible_relation_oid(reference)?
        }
        .ok_or_else(|| format!("relation \"{reference}\" does not exist"))?;
        **expr = Expr::TypedLiteral {
            value: Value::Int(oid),
            ty: "regclass".into(),
        };
        changed = true;
        Ok(())
    })?;
    Ok(changed)
}

pub fn bind_table_schema_regclass_constants(
    catalog: &dyn SchemaReferenceCatalog,
    columns: &mut [ColumnDef],
    checks: &mut [TableCheck],
    loaded: bool,
) -> Result<bool, String> {
    let mut changed = false;
    for column in columns {
        for expression in [&mut column.default, &mut column.check]
            .into_iter()
            .flatten()
        {
            changed |= bind_schema_regclass_constants(catalog, expression, loaded)?;
        }
        if let Some(generated) = &mut column.generated {
            changed |= bind_schema_regclass_constants(catalog, &mut generated.expression, loaded)?;
        }
    }
    for check in checks {
        changed |= bind_schema_regclass_constants(catalog, &mut check.expr, loaded)?;
    }
    Ok(changed)
}
pub fn bind_sequence_references_in_expr(
    catalog: &dyn SchemaReferenceCatalog,
    expression: &mut Expr,
) -> Result<(), String> {
    super::rewrites::rewrite_sequence_function_references(expression, &mut |reference| {
        *reference = catalog.sequence_for_binding(reference)?;
        Ok(())
    })?;
    bind_schema_regclass_constants(catalog, expression, false)?;
    Ok(())
}
