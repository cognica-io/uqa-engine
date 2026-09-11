//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared namespace binding for relation privilege declarations.
use crate::SQLError;
pub trait GrantNamespace {
    fn temporary_schema_name(&self) -> String;
    fn temporary_namespace_allocated(&self) -> bool;
    fn has_namespace(&self, name: &str) -> Result<bool, String>;
}
pub fn bind_grant_schemas(
    namespace: &dyn GrantNamespace,
    schemas: &[String],
) -> Result<Vec<String>, SQLError> {
    let temporary_schema = namespace.temporary_schema_name();
    let mut resolved_schemas = Vec::with_capacity(schemas.len());
    for schema in schemas {
        let resolved = if schema == "pg_temp" {
            temporary_schema.clone()
        } else {
            schema.clone()
        };
        let exists = if resolved == temporary_schema {
            namespace.temporary_namespace_allocated()
        } else {
            namespace.has_namespace(&resolved).map_err(|error| {
                SQLError::Internal(format!("resolve schema `{schema}`: {error}"))
            })?
        };
        if !exists {
            return Err(SQLError::Routine {
                sqlstate: "3F000".into(),
                message: format!("schema \"{schema}\" does not exist"),
            });
        }
        if !resolved_schemas.contains(&resolved) {
            resolved_schemas.push(resolved);
        }
    }
    Ok(resolved_schemas)
}
