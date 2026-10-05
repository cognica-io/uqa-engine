//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `ALTER ... SET SCHEMA` destinations after the owning relation and namespace are locked.

use super::RelationAlterNames;
use crate::{ast::RelationPersistence, SQLError};
use uqa_core::RelationIdentity;

pub fn declared_target(
    source: &RelationIdentity,
    schema: &str,
) -> Result<RelationIdentity, SQLError> {
    let (qualifier, schema) =
        RelationIdentity::parse_reference(schema).map_err(SQLError::Internal)?;
    if qualifier.is_some() {
        return Err(SQLError::Internal(
            "SET SCHEMA produced a qualified schema".into(),
        ));
    }
    Ok(RelationIdentity::new(schema, &source.name))
}

/// `CheckSetNamespace` precedes the unchanged-namespace shortcut and relation-name collision checks.
pub fn validate_target(
    catalog: &dyn RelationAlterNames,
    source: &RelationIdentity,
    target: &RelationIdentity,
    persistence: RelationPersistence,
    temporary_schema: &str,
) -> Result<bool, SQLError> {
    let temporary = |schema: &str| {
        schema == temporary_schema
            || schema.starts_with("pg_temp_")
            || schema.starts_with("pg_toast_temp_")
    };
    if persistence == RelationPersistence::Temporary
        || temporary(&source.schema)
        || temporary(&target.schema)
    {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot move objects into or out of temporary schemas".into(),
        });
    }
    if source.schema == "pg_toast" || target.schema == "pg_toast" {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot move objects into or out of TOAST schema".into(),
        });
    }
    if source == target {
        return Ok(false);
    }
    ensure_name_available(catalog, target)?;
    Ok(true)
}

pub fn ensure_name_available(
    catalog: &dyn RelationAlterNames,
    target: &RelationIdentity,
) -> Result<(), SQLError> {
    if catalog
        .relation_kind_at(&target.qualified_name())
        .map_err(SQLError::Internal)?
        .is_some()
    {
        return Err(SQLError::Routine {
            sqlstate: "42P07".into(),
            message: format!(
                "relation \"{}\" already exists in schema \"{}\"",
                target.name, target.schema
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
