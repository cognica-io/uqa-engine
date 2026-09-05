//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index definition lookup through the current catalog snapshot.

use super::helpers::index_definitions::indexdef;
use crate::Engine;
use uqa_core::Value;
use uqa_sql::{expr::quote_ident, SQLError};

pub(in crate::sql) fn pg_get_indexdef_value(
    engine: &Engine,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    if !matches!(arguments.len(), 1 | 3) {
        return Err(SQLError::BadArity {
            name: "pg_get_indexdef".into(),
            expected: "1 or 3".into(),
            actual: arguments.len(),
        });
    }
    if arguments.iter().any(|value| matches!(value, Value::Null)) {
        return Ok(Value::Null);
    }
    let Value::Int(oid) = arguments[0] else {
        return Err(SQLError::TypeMismatch(
            "pg_get_indexdef requires oid".into(),
        ));
    };
    let (column, pretty) = match arguments {
        [_] => (0, false),
        [_, Value::Int(column), Value::Bool(pretty)] => (*column, *pretty),
        _ => {
            return Err(SQLError::TypeMismatch(
                "pg_get_indexdef requires oid, integer, boolean".into(),
            ))
        }
    };
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
    let Some(index) = super::pg_catalog::catalog_index_relations(&catalog, &resolution)?
        .into_iter()
        .find(|index| index.oid() == oid)
    else {
        return Ok(Value::Null);
    };
    if column != 0 {
        return Ok(Value::Str(
            usize::try_from(column - 1)
                .ok()
                .and_then(|position| {
                    index
                        .columns
                        .iter()
                        .chain(&index.definition.included_columns)
                        .nth(position)
                })
                .map_or_else(String::new, |name| quote_ident(name)),
        ));
    }
    let relation =
        crate::RelationIdentity::from_legacy_name(&index.table_name).map_err(SQLError::Internal)?;
    let visible = catalog
        .relation_kind_resolution(&resolution, &quote_ident(&relation.name))?
        .into_found()
        .is_some_and(|(table, _)| table == index.table_name);
    let mut target = if pretty && visible {
        quote_ident(&relation.name)
    } else {
        let schema = if relation.schema.starts_with("pg_temp_") {
            "pg_temp"
        } else {
            &relation.schema
        };
        format!("{}.{}", quote_ident(schema), quote_ident(&relation.name))
    };
    if index.relkind == "I" {
        target = format!("ONLY {target}");
    }
    indexdef(&catalog, &resolution, &index, &target, pretty).map(Value::Str)
}
