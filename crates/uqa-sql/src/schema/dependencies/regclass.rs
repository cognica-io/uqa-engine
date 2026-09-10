//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind literal relation references in persisted SQL schema expressions.
use super::walk_schema_expr_mut;
use crate::ast::{ColumnDef, Expr, TableCheck};
use uqa_core::Value;

pub trait SchemaReferenceCatalog {
    fn loaded_relation_name(&self, reference: &str) -> Result<Option<String>, String>;
    fn bound_relation_oid(&self, canonical: &str) -> Result<Option<i64>, String>;
    fn visible_relation_oid(&self, reference: &str) -> Result<Option<i64>, String>;
    fn sequence_for_binding(&self, reference: &str) -> Result<String, String>;
}

pub fn regclass_constant_oid(expression: &Expr) -> Option<i64> {
    match expression {
        Expr::TypedLiteral {
            value: Value::Int(oid),
            ty,
        } if is_regclass(ty) => Some(*oid),
        _ => None,
    }
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
