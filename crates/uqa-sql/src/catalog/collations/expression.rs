//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collation-clause identity and type validation shared by expression analysis and catalog encoding.

use crate::ast::{Expr, FunctionDispatch};
use crate::{ColumnType, FunctionTypeResolver, SQLError, ScalarExpr};
use uqa_core::{memory::ProductionControl, Value};

pub struct Clause<'a, T> {
    pub argument: &'a T,
    pub name: &'a str,
    pub oid: Option<u32>,
}

impl<T> Clause<'_, T> {
    /// Executable SQL retains a selected identity instead of resolving its former spelling again.
    pub fn executable_name(&self) -> Result<std::borrow::Cow<'_, str>, SQLError> {
        let Some(oid) = self.oid else {
            return Ok(std::borrow::Cow::Borrowed(self.name));
        };
        let name = super::builtin_collation_name(i64::from(oid))
            .ok_or_else(|| SQLError::Internal("unknown collation identity".into()))?;
        Ok(std::borrow::Cow::Owned(format!(
            "pg_catalog.{}",
            crate::expr::quote_ident(name)
        )))
    }
}

/// Retained constants may carry an indeterminate collatable result, but cannot attach a collation to a noncollatable or unresolved type.
pub(crate) fn validate_literal_collation(
    ty: Option<&ColumnType>,
    oid: Option<u32>,
) -> Result<(), SQLError> {
    if oid.is_some_and(|oid| oid != 0)
        && ty.is_none_or(|ty| !crate::schema::composites::type_is_collatable(ty))
    {
        return Err(SQLError::Internal(
            "constant collation does not match its declared type".into(),
        ));
    }
    Ok(())
}

pub fn scalar_clause(expression: &ScalarExpr) -> Result<Option<Clause<'_, ScalarExpr>>, SQLError> {
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = expression
    else {
        return Ok(None);
    };
    let Some(FunctionDispatch::Collate { oid }) = binding.dispatch else {
        return Ok(None);
    };
    let [argument, ScalarExpr::Literal(Value::Str(name))] = args.as_slice() else {
        return Err(SQLError::Internal("invalid COLLATE expression".into()));
    };
    Ok(Some(Clause {
        argument,
        name,
        oid,
    }))
}

pub fn ast_clause(expression: &Expr) -> Result<Option<Clause<'_, Expr>>, SQLError> {
    let Expr::Func {
        binding: Some(binding),
        args,
        ..
    } = expression
    else {
        return Ok(None);
    };
    let Some(FunctionDispatch::Collate { oid }) = binding.dispatch else {
        return Ok(None);
    };
    let [argument, Expr::Literal(Value::Str(name))] = args.as_slice() else {
        return Err(SQLError::Internal("invalid COLLATE expression".into()));
    };
    Ok(Some(Clause {
        argument,
        name,
        oid,
    }))
}

pub(crate) fn discard_scalar_clause(expression: &mut ScalarExpr) -> Result<bool, SQLError> {
    if scalar_clause(expression)?.is_none() {
        return Ok(false);
    }
    let ScalarExpr::Func { args, .. } =
        std::mem::replace(expression, ScalarExpr::Literal(Value::Null))
    else {
        unreachable!("validated collation clause");
    };
    *expression = args
        .into_iter()
        .next()
        .expect("validated collation argument");
    Ok(true)
}

pub(crate) fn discard_ast_clause(expression: &mut Expr) -> Result<bool, SQLError> {
    if ast_clause(expression)?.is_none() {
        return Ok(false);
    }
    let Expr::Func { args, .. } = std::mem::replace(expression, Expr::Literal(Value::Null)) else {
        unreachable!("validated collation clause");
    };
    *expression = args
        .into_iter()
        .next()
        .expect("validated collation argument");
    Ok(true)
}

pub fn select(
    name: &str,
    selected: Option<u32>,
    ty: Option<&ColumnType>,
    catalog: Option<&dyn FunctionTypeResolver>,
) -> Result<u32, SQLError> {
    select_with_control(
        name,
        selected,
        ty,
        catalog,
        &ProductionControl::uncontrolled(),
    )
}

pub(crate) fn select_with_control(
    name: &str,
    selected: Option<u32>,
    ty: Option<&ColumnType>,
    catalog: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<u32, SQLError> {
    control.check()?;
    assert!(
        catalog.is_none() || control.budget().is_none(),
        "catalog callbacks are resolved outside retained generated inference"
    );
    if let Some(ty) = ty.filter(|ty| !crate::schema::composites::type_is_collatable(ty)) {
        let ty = ty.without_type_modifiers_with_control(control)?;
        let name = match catalog {
            Some(catalog) => control.finish(catalog.format_type_name(&ty)?, None)?,
            None => ty.display_name_with_control(control)?,
        };
        return Err(diagnostic(
            control,
            "42804",
            format_args!("collations are not supported by type {}", name.as_str()),
        )?);
    }
    if let Some(oid) = selected {
        return valid_oid(oid);
    }
    if let Some(oid) = catalog
        .map(|catalog| catalog.resolve_collation_name(name))
        .transpose()?
        .flatten()
    {
        return valid_oid(oid);
    }
    builtin_name(name, control)
}

fn valid_oid(oid: u32) -> Result<u32, SQLError> {
    if oid == 0 {
        Err(SQLError::Internal("invalid collation identity".into()))
    } else {
        Ok(oid)
    }
}

/// Catalog-free analysis has the predefined namespace and collation identities.
fn builtin_name(name: &str, control: &ProductionControl<'_>) -> Result<u32, SQLError> {
    let names = crate::compiler::parse_regobject_name_with_control(name, control)?
        .ok_or_else(|| SQLError::Internal("malformed COLLATE name".into()))?;
    let (schema, local) = match names.as_slice() {
        [local] => (None, local),
        [schema, local] => (Some(schema.as_str()), local),
        [database, schema, local] if database == "uqa" => (Some(schema.as_str()), local),
        [_, _, _] => {
            return Err(diagnostic(
                control,
                "0A000",
                format_args!(
                    "cross-database references are not implemented: {}",
                    QualifiedName(&names)
                ),
            )?)
        }
        _ => {
            return Err(diagnostic(
                control,
                "42601",
                format_args!(
                    "improper qualified name (too many dotted names): {}",
                    QualifiedName(&names)
                ),
            )?)
        }
    };
    if let Some(schema) = schema.filter(|schema| !matches!(*schema, "pg_catalog" | "public")) {
        return Err(diagnostic(
            control,
            "3F000",
            format_args!("schema \"{schema}\" does not exist"),
        )?);
    }
    if schema.is_none_or(|schema| schema == "pg_catalog") {
        if let Some(oid) = super::builtin_collation_oid(local) {
            return u32::try_from(oid)
                .map_err(|_| SQLError::Internal("invalid collation identity".into()));
        }
    }
    Err(diagnostic(
        control,
        "42704",
        format_args!(
            "collation \"{}\" for encoding \"UTF8\" does not exist",
            QualifiedName(&names)
        ),
    )?)
}

struct QualifiedName<'a>(&'a [String]);

impl std::fmt::Display for QualifiedName<'_> {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (position, component) in self.0.iter().enumerate() {
            if position > 0 {
                output.write_str(".")?;
            }
            output.write_str(component)?;
        }
        Ok(())
    }
}

fn diagnostic(
    control: &ProductionControl<'_>,
    sqlstate: &str,
    message: std::fmt::Arguments<'_>,
) -> Result<SQLError, SQLError> {
    // Error payloads leave this producer through the existing unreserved SQLError boundary after admitted construction.
    let (message, _memory) = control.format(message)?.into_parts();
    Ok(SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    })
}

#[cfg(test)]
mod tests;
