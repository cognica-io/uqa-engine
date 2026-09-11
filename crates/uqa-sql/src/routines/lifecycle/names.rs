//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Search-path and namespace authorization rules for routine names.

use crate::{catalog::security::SchemaSecurity, SQLError};
use uqa_core::RelationIdentity;

pub trait RoutineNameCatalog {
    fn schema_security(&self, schema: &str) -> Option<SchemaSecurity>;
    fn current_user_name(&self) -> String;
    fn search_path(&self) -> Vec<String>;
    fn require_schema_usage(&self, schema: &str, role: &str) -> Result<(), SQLError>;
    fn schema_has_usage(&self, schema: &str, role: &str) -> bool;
}

pub fn routine_lookup_keys(
    catalog: &dyn RoutineNameCatalog,
    name: &str,
) -> Result<Vec<String>, SQLError> {
    let (schema, local_name) =
        RelationIdentity::parse_reference(name).map_err(|error| SQLError::Routine {
            sqlstate: "42602".into(),
            message: format!("invalid routine name `{name}`: {error}"),
        })?;
    if let Some(schema) = schema {
        if catalog.schema_security(&schema).is_none() {
            return Err(SQLError::Routine {
                sqlstate: "3F000".into(),
                message: format!("schema \"{schema}\" does not exist"),
            });
        }
        catalog.require_schema_usage(&schema, &catalog.current_user_name())?;
        return Ok(vec![
            RelationIdentity::new(schema, local_name).qualified_name()
        ]);
    }
    let current_user = catalog.current_user_name();
    let search_path = catalog.search_path();
    Ok(search_path
        .into_iter()
        .filter(|schema| {
            catalog.schema_security(schema).is_some()
                && catalog.schema_has_usage(schema, &current_user)
        })
        .map(|schema| RelationIdentity::new(schema, &local_name).qualified_name())
        .collect())
}

/// Defer namespace errors during recursive argument analysis; definitive binding checks again.
pub fn routine_lookup_keys_for_analysis(
    catalog: &dyn RoutineNameCatalog,
    name: &str,
) -> Result<Option<Vec<String>>, SQLError> {
    match routine_lookup_keys(catalog, name) {
        Err(error) if crate::routines::is_routine_namespace_lookup_error(&error) => Ok(None),
        result => result.map(Some),
    }
}
