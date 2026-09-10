//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable catalog metadata supplied to the native PL/pgSQL parser.

use uqa_core::Value;
use uqa_sql::plpgsql::{PlpgsqlCatalog, PlpgsqlType};
use uqa_sql::{ResultRow, SQLError};

use crate::catalog::{CatalogReadView, RelationNameResolution};

pub fn plpgsql_catalog(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    search_path: Vec<String>,
) -> Result<PlpgsqlCatalog, SQLError> {
    let namespaces = super::build_pg_namespace(catalog, resolution)?
        .iter()
        .map(|row| Ok((text(row, "nspname")?.to_string(), oid(row, "oid")?)))
        .collect::<Result<_, SQLError>>()?;
    let types = super::build_pg_type(catalog)
        .iter()
        .map(|row| {
            Ok(PlpgsqlType {
                oid: oid(row, "oid")?,
                namespace_oid: oid(row, "typnamespace")?,
                name: text(row, "typname")?.to_string(),
                length: i16::try_from(integer(row, "typlen")?)
                    .map_err(|_| invalid_metadata("typlen"))?,
                by_value: row.get("typbyval") == Some(&Value::Bool(true)),
                type_kind: character(row, "typtype")?,
                category: character(row, "typcategory")?,
                alignment: character(row, "typalign")?,
                storage: character(row, "typstorage")?,
                array_oid: oid(row, "typarray")?,
                element_oid: oid(row, "typelem")?,
                base_type_oid: oid(row, "typbasetype")?,
                collation_oid: oid(row, "typcollation")?,
                subscript_handler_oid: oid(row, "typsubscript")?,
            })
        })
        .collect::<Result<_, SQLError>>()?;
    Ok(PlpgsqlCatalog {
        namespaces,
        search_path,
        types,
    })
}

fn integer(row: &ResultRow, field: &str) -> Result<i64, SQLError> {
    match row.get(field) {
        Some(Value::Int(value)) => Ok(*value),
        _ => Err(invalid_metadata(field)),
    }
}

fn oid(row: &ResultRow, field: &str) -> Result<u32, SQLError> {
    u32::try_from(integer(row, field)?).map_err(|_| invalid_metadata(field))
}

fn text<'a>(row: &'a ResultRow, field: &str) -> Result<&'a str, SQLError> {
    match row.get(field) {
        Some(Value::Str(value)) => Ok(value),
        _ => Err(invalid_metadata(field)),
    }
}

fn character(row: &ResultRow, field: &str) -> Result<u8, SQLError> {
    match text(row, field)?.as_bytes() {
        [value] => Ok(*value),
        _ => Err(invalid_metadata(field)),
    }
}

fn invalid_metadata(field: &str) -> SQLError {
    SQLError::Internal(format!("invalid PL/pgSQL parser catalog field {field}"))
}
