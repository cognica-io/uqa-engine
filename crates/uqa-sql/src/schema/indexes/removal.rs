//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP INDEX names, constraint dependencies and stored field references.
use crate::{
    ast::{ColumnType, ForeignKey},
    catalog::resolution::RelationResolution,
    SQLError,
};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;

pub fn resolve_drop_index_name(
    resolution: RelationResolution,
    requested: &str,
    if_exists: bool,
    notice: &mut dyn FnMut(&str),
) -> Result<Option<String>, SQLError> {
    match resolution {
        RelationResolution::Found(canonical, "index") => Ok(Some(canonical)),
        RelationResolution::Found(_, _) => Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("\"{requested}\" is not an index"),
        }),
        RelationResolution::MissingSchema(schema) if if_exists => {
            notice(&format!("schema \"{schema}\" does not exist, skipping"));
            Ok(None)
        }
        RelationResolution::MissingSchema(schema) => Err(SQLError::Routine {
            sqlstate: "3F000".into(),
            message: format!("schema \"{schema}\" does not exist"),
        }),
        RelationResolution::MissingRelation if if_exists => {
            let local = uqa_core::RelationIdentity::parse_reference(requested)
                .map_err(SQLError::Internal)?
                .1;
            notice(&format!("index \"{local}\" does not exist, skipping"));
            Ok(None)
        }
        RelationResolution::MissingRelation => {
            let local = uqa_core::RelationIdentity::parse_reference(requested)
                .map_err(SQLError::Internal)?
                .1;
            Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("index \"{local}\" does not exist"),
            })
        }
    }
}
pub fn ensure_index_not_constraint_owned(
    relation: &RelationIdentity,
    table: &str,
    constraint_owned: bool,
) -> Result<(), SQLError> {
    if constraint_owned {
        return Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: format!(
                "cannot drop index {} because constraint {} on table {} requires it",
                relation.name, relation.name, table
            ),
        });
    }
    Ok(())
}
pub fn catalog_index_columns(
    relation: &RelationIdentity,
    columns_json: &str,
    action: &str,
) -> Result<Vec<String>, SQLError> {
    serde_json::from_str(columns_json).map_err(|e| {
        SQLError::Internal(format!(
            "{action} `{}`: invalid index column metadata: {e}",
            relation.qualified_name()
        ))
    })
}

pub fn collect_index_dependents(
    index_name: &str,
    referrers: Vec<(String, ForeignKey)>,
    cascade: bool,
    dependents: &mut BTreeSet<(String, String)>,
) -> Result<(), SQLError> {
    for (table, foreign_key) in referrers {
        if foreign_key.referenced_key.as_deref() != Some(index_name) {
            continue;
        }
        let name = foreign_key
            .name
            .ok_or_else(|| SQLError::Internal("unnamed foreign-key dependency".into()))?;
        if !cascade {
            return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!("cannot drop index {index_name} because constraint {name} on table {table} depends on it"),
                });
        }
        dependents.insert((table, name));
    }
    Ok(())
}
/// Borrowed catalog metadata used to detect remaining references to a physical text field.
pub struct IndexRemovalCandidate<'a> {
    pub relation: &'a RelationIdentity,
    pub table: &'a str,
    pub method: &'a str,
    pub columns_json: &'a str,
}
pub fn gin_field_is_referenced<'a>(
    relation: &RelationIdentity,
    table: &str,
    field: &str,
    candidates: impl IntoIterator<Item = IndexRemovalCandidate<'a>>,
) -> Result<bool, SQLError> {
    for candidate in candidates {
        if candidate.relation == relation
            || candidate.table != table
            || !candidate.method.eq_ignore_ascii_case("gin")
        {
            continue;
        }
        if catalog_index_columns(candidate.relation, candidate.columns_json, "DROP INDEX")?
            .iter()
            .any(|candidate_field| candidate_field == field)
        {
            return Ok(true);
        }
    }
    Ok(false)
}
pub fn vector_index_dimensions(
    relation: &RelationIdentity,
    table: &str,
    column: &str,
    column_type: Option<ColumnType>,
) -> Result<u32, SQLError> {
    match column_type {
        Some(ColumnType::Vector(dim) | ColumnType::Tensor(dim)) => Ok(dim),
        Some(other) => Err(SQLError::Unsupported(format!(
            "DROP INDEX `{}`: vector-index column `{}`.`{column}` is no longer VECTOR or TENSOR, got {other:?}",
            relation.qualified_name(), table
        ))),
        None => Err(SQLError::Unsupported(format!(
            "DROP INDEX `{}`: column `{}`.`{column}` does not exist",
            relation.qualified_name(), table
        ))),
    }
}
