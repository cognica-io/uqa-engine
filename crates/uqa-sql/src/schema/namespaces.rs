//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Namespace declaration rules independent of catalog storage.

pub mod removal;

pub fn validate_schema_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("invalid schema name `{name}`"));
    }
    if crate::catalog::is_virtual_system_schema(name) {
        return Err(format!("schema name `{name}` is reserved"));
    }
    Ok(())
}

pub mod creation;
