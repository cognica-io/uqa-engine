//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation-bound relation constants in stored column and constraint expressions.

use uqa_core::Value;
use uqa_sql::ast::{ColumnDef, Expr, TableCheck};

use super::super::walk_schema_expr_mut;
use crate::{Engine, StorageBackendError, StorageBackendResult};

pub(super) fn regclass_constant_oid(expression: &Expr) -> Option<i64> {
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

impl Engine {
    pub(crate) fn bind_schema_regclass_constants(
        &self,
        expression: &mut Expr,
        loaded: bool,
    ) -> StorageBackendResult<bool> {
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
                    Err(_) => match self
                        .resolve_loaded_visible_relation_kind(reference)
                        .map_err(|error| StorageBackendError::Other(error.to_string()))?
                        .into_found()
                    {
                        Some((canonical, _)) => {
                            crate::sql::resolve_bound_regclass_oid(self, &canonical)
                                .map_err(|error| StorageBackendError::Other(error.to_string()))?
                        }
                        None => None,
                    },
                }
            } else {
                crate::sql::resolve_regclass_oid(self, reference)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?
            }
            .ok_or_else(|| {
                StorageBackendError::Other(format!("relation \"{reference}\" does not exist"))
            })?;
            **expr = Expr::TypedLiteral {
                value: Value::Int(oid),
                ty: "regclass".into(),
            };
            changed = true;
            Ok(())
        })?;
        Ok(changed)
    }

    pub(in crate::table_storage) fn bind_table_schema_regclass_constants(
        &self,
        columns: &mut [ColumnDef],
        checks: &mut [TableCheck],
        loaded: bool,
    ) -> StorageBackendResult<bool> {
        let mut changed = false;
        for column in columns {
            for expression in [&mut column.default, &mut column.check]
                .into_iter()
                .flatten()
            {
                changed |= self.bind_schema_regclass_constants(expression, loaded)?;
            }
            if let Some(generated) = &mut column.generated {
                changed |=
                    self.bind_schema_regclass_constants(&mut generated.expression, loaded)?;
            }
        }
        for check in checks {
            changed |= self.bind_schema_regclass_constants(&mut check.expr, loaded)?;
        }
        Ok(changed)
    }
}
