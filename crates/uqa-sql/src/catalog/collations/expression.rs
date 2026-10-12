//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collation-clause identity and type validation shared by expression analysis and catalog encoding.

use crate::ast::{Expr, FunctionDispatch};
use crate::{ColumnType, FunctionTypeResolver, SQLError, ScalarExpr};
use uqa_core::Value;

pub struct Clause<'a, T> {
    pub argument: &'a T,
    pub name: &'a str,
    pub oid: Option<u32>,
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
    if let Some(ty) = ty.filter(|ty| !crate::schema::composites::type_is_collatable(ty)) {
        let ty = ty.without_type_modifiers();
        let name = catalog.map_or_else(
            || Ok(ty.display_name()),
            |catalog| catalog.format_type_name(&ty),
        )?;
        return Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: format!("collations are not supported by type {name}"),
        });
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
    builtin_name(name)
}

fn valid_oid(oid: u32) -> Result<u32, SQLError> {
    if oid == 0 {
        Err(SQLError::Internal("invalid collation identity".into()))
    } else {
        Ok(oid)
    }
}

/// Catalog-free analysis has the predefined namespace and collation identities.
fn builtin_name(name: &str) -> Result<u32, SQLError> {
    let names = crate::parse_regobject_name(name)
        .ok_or_else(|| SQLError::Internal("malformed COLLATE name".into()))?;
    let (schema, local) = match names.as_slice() {
        [local] => (None, local),
        [schema, local] => (Some(schema.as_str()), local),
        [database, schema, local] if database == "uqa" => (Some(schema.as_str()), local),
        [_, _, _] => {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: format!(
                    "cross-database references are not implemented: {}",
                    names.join(".")
                ),
            })
        }
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: format!(
                    "improper qualified name (too many dotted names): {}",
                    names.join(".")
                ),
            })
        }
    };
    if let Some(schema) = schema.filter(|schema| !matches!(*schema, "pg_catalog" | "public")) {
        return Err(SQLError::Routine {
            sqlstate: "3F000".into(),
            message: format!("schema \"{schema}\" does not exist"),
        });
    }
    if schema.is_none_or(|schema| schema == "pg_catalog") {
        if let Some(oid) = super::builtin_collation_oid(local) {
            return u32::try_from(oid)
                .map_err(|_| SQLError::Internal("invalid collation identity".into()));
        }
    }
    Err(SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!(
            "collation \"{}\" for encoding \"UTF8\" does not exist",
            names.join(".")
        ),
    })
}
