//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Search-path and namespace authorization rules for routine names.

use crate::catalog::roles::RoleReference;
use crate::{catalog::security::BoundSchemaSecurity, SQLError};
use uqa_core::RelationIdentity;

/// Immutable routine address and declared input types, independent of invocation coercions.
pub struct RoutineCatalogIdentity {
    pub oid: u32,
    pub relation: RelationIdentity,
    pub argument_types: Vec<i64>,
    pub kind: char,
}

pub trait RoutineNameCatalog {
    fn schema_security(&self, schema: &str) -> Option<BoundSchemaSecurity>;
    fn current_role(&self) -> RoleReference;
    fn search_path(&self) -> Vec<String>;
    fn require_schema_usage(&self, schema: &str, role: &RoleReference) -> Result<(), SQLError>;
    fn schema_has_usage(&self, schema: &str, role: &RoleReference) -> bool;
    /// `format_type_be` of a routine argument type: a user-defined type, recorded by identity, is spelled by its current name.
    fn routine_type_display(&self, type_name: &str) -> String;
    /// `format_procedure` of an existing routine, including search-path qualification and its current input type names. Call before retaining the routine registry's write guard.
    fn routine_identity_display(&self, oid: u32) -> Result<String, SQLError>;
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
        catalog.require_schema_usage(&schema, &catalog.current_role())?;
        return Ok(vec![
            RelationIdentity::new(schema, local_name).qualified_name()
        ]);
    }
    let current_user = catalog.current_role();
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

/// Catalog command lookup includes implicit `pg_catalog` in `PostgreSQL`'s search-path position.
pub fn routine_lookup_keys_with_builtins(
    catalog: &dyn RoutineNameCatalog,
    name: &str,
) -> Result<Vec<String>, SQLError> {
    let mut keys = routine_lookup_keys(catalog, name)?;
    let (schema, local) = RelationIdentity::parse_reference(name).map_err(SQLError::Internal)?;
    if schema.is_none()
        && catalog.schema_has_usage("pg_catalog", &catalog.current_role())
        && !catalog
            .search_path()
            .iter()
            .any(|schema| schema == "pg_catalog")
    {
        keys.insert(
            0,
            RelationIdentity::new("pg_catalog", local).qualified_name(),
        );
    }
    Ok(keys)
}

/// `LookupFuncName` matches declared input OIDs exactly, ignores procedures, and does not expand defaults or variadics.
pub fn exact_function(
    catalog: &dyn RoutineNameCatalog,
    identities: &[RoutineCatalogIdentity],
    name: &str,
    argument_types: &[i64],
    display_types: &[String],
) -> Result<usize, SQLError> {
    for key in routine_lookup_keys_with_builtins(catalog, name)? {
        if let Some(index) = identities.iter().position(|identity| {
            identity.relation.qualified_name() == key
                && identity.kind != 'p'
                && identity.argument_types == argument_types
        }) {
            return Ok(index);
        }
    }
    Err(SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!(
            "function {} does not exist",
            super::routine_signature_display(catalog, name, display_types)
        ),
    })
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

#[cfg(test)]
mod tests;
