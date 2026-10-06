//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in SQL bodies are reconstructed from their typed catalog definitions.

use uqa_core::Value;
use uqa_sql::{
    catalog::node_tree::{deparse, parse},
    expr::quote_ident,
    SQLError,
};

use crate::catalog::context::CatalogContext;

use super::super::{builtin_routines::BuiltinRoutineCatalogEntry, format_type_value, regtypes};

pub(super) fn definition(
    context: &CatalogContext<'_>,
    routine: &BuiltinRoutineCatalogEntry,
) -> Result<Value, SQLError> {
    let Some(body) = routine.sql_body() else {
        return Ok(Value::Null);
    };
    let body = parse(&body)?;
    deparse::return_body(&body, &Names { context, routine }).map(Value::Str)
}

struct Names<'a, 'b> {
    context: &'a CatalogContext<'b>,
    routine: &'a BuiltinRoutineCatalogEntry,
}

impl deparse::ExpressionNames for Names<'_, '_> {
    fn column(&self, _: i64) -> Result<String, SQLError> {
        Err(SQLError::Internal(
            "built-in RETURN body contains a relation column".into(),
        ))
    }

    fn routine(&self, oid: i64) -> Result<Vec<String>, SQLError> {
        regtypes::routine_name_parts(self.context, oid)?.ok_or_else(|| {
            SQLError::Internal(format!(
                "built-in RETURN body references unknown routine OID {oid}"
            ))
        })
    }

    fn type_name(&self, oid: i64, modifier: i64) -> Result<String, SQLError> {
        match format_type_value(self.context, &[Value::Int(oid), Value::Int(modifier)])? {
            Value::Str(name) => Ok(name),
            _ => Err(SQLError::Internal(
                "built-in RETURN body type has no catalog name".into(),
            )),
        }
    }

    fn parameter(&self, number: i64) -> Result<String, SQLError> {
        let index = usize::try_from(number)
            .ok()
            .and_then(|number| number.checked_sub(1))
            .filter(|index| *index < self.routine.argument_types.len())
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "built-in RETURN body references unknown parameter ${number}"
                ))
            })?;
        Ok(self
            .routine
            .argument_names
            .get(index)
            .filter(|name| !name.is_empty())
            .map_or_else(|| format!("${number}"), |name| quote_ident(name)))
    }
}
