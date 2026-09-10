//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! COPY stream envelope validation and relation-column binding.
use super::{CopyDirection, CopyEndpoint, CopyFormat, CopyStatement};
use crate::{assignment::columns::ColumnCatalogError, ast::ColumnDef, SQLError};
use std::collections::BTreeSet;
/// Metadata from the relation generation selected before COPY column validation.
pub trait CopyRelation {
    fn columns(&self) -> Vec<ColumnDef>;
    fn is_partitioned(&self) -> bool;
}
pub trait CopyCatalog {
    fn resolve_relation_kind(&self, name: &str)
        -> Result<Option<(String, &'static str)>, SQLError>;
    fn table(&self, name: &str) -> Result<Option<Box<dyn CopyRelation + '_>>, ColumnCatalogError>;
}
pub fn validate_stream(copy: &CopyStatement, direction: CopyDirection) -> Result<(), SQLError> {
    ensure_copy_direction(copy, direction)?;
    ensure_stdio_endpoint(copy)?;
    ensure_stream_format(copy)
}
pub fn relation_columns(
    catalog: &dyn CopyCatalog,
    relation: &str,
    display_name: &str,
    requested: &[String],
    reject_partitioned_output: bool,
) -> Result<(String, Vec<String>), SQLError> {
    let canonical = match catalog.resolve_relation_kind(relation)? {
        Some((canonical, "table")) => canonical,
        Some(_) | None => return Err(SQLError::UnknownTable(relation.to_string())),
    };
    let table = catalog
        .table(&canonical)
        .map_err(|error| SQLError::Internal(format!("read COPY relation `{canonical}`: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(relation.to_string()))?;
    let definitions = table.columns();
    let columns = if requested.is_empty() {
        definitions
            .into_iter()
            .filter(|column| column.generated.is_none())
            .map(|column| column.name)
            .collect()
    } else {
        let mut seen = BTreeSet::new();
        let mut columns = Vec::with_capacity(requested.len());
        for requested in requested {
            if !seen.insert(requested.clone()) {
                return Err(SQLError::Routine {
                    sqlstate: "42701".into(),
                    message: format!("column \"{requested}\" specified more than once"),
                });
            }
            let Some(column) = definitions.iter().find(|column| column.name == *requested) else {
                return Err(SQLError::Routine {
                    sqlstate: "42703".into(),
                    message: format!(
                        "column \"{requested}\" of relation \"{display_name}\" does not exist"
                    ),
                });
            };
            if column.generated.is_some() {
                return Err(SQLError::Routine {
                        sqlstate: "42P10".into(),
                        message: format!(
                            "column \"{requested}\" is a generated column\nDETAIL: Generated columns cannot be used in COPY."
                        ),
                    });
            }
            columns.push(requested.clone());
        }
        columns
    };
    if reject_partitioned_output && table.is_partitioned() {
        return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!(
                    "cannot copy from partitioned table \"{display_name}\"\nHINT: Try the COPY (SELECT ...) TO variant."
                ),
            });
    }
    Ok((canonical, columns))
}
fn ensure_copy_direction(copy: &CopyStatement, expected: CopyDirection) -> Result<(), SQLError> {
    if copy.direction == expected {
        return Ok(());
    }
    let expected = match expected {
        CopyDirection::From => "FROM STDIN",
        CopyDirection::To => "TO STDOUT",
    };
    Err(SQLError::Routine {
        sqlstate: "42601".into(),
        message: format!("COPY stream API requires COPY {expected}"),
    })
}

fn ensure_stdio_endpoint(copy: &CopyStatement) -> Result<(), SQLError> {
    match &copy.endpoint {
        CopyEndpoint::Stdio => Ok(()),
        CopyEndpoint::File(_) => Err(SQLError::Unsupported(
            "server-side COPY files are not available through the embedded stream API".into(),
        )),
        CopyEndpoint::Program(_) => Err(SQLError::Unsupported(
            "COPY PROGRAM is not available through the embedded stream API".into(),
        )),
    }
}

fn ensure_stream_format(copy: &CopyStatement) -> Result<(), SQLError> {
    if copy.options.format == CopyFormat::Binary {
        Err(SQLError::Unsupported(
            "binary COPY format is not implemented".into(),
        ))
    } else {
        Ok(())
    }
}
