//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Namespace declaration rules independent of catalog storage.

pub mod removal;

pub fn validate_schema_name(name: &str) -> Result<(), String> {
    validate_stored_schema_name(name)?;
    if crate::catalog::is_virtual_system_schema(name) {
        return Err(format!("schema name `{name}` is reserved"));
    }
    Ok(())
}

/// Existing namespaces may persist owner and ACL overrides even when new declarations cannot use their names.
pub fn validate_stored_schema_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        Err(format!("invalid schema name `{name}`"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_system_namespace_security_is_valid_without_permitting_reserved_creation() {
        for name in ["pg_catalog", "information_schema", "ag_catalog"] {
            validate_stored_schema_name(name).unwrap();
            assert!(validate_schema_name(name).unwrap_err().contains("reserved"));
        }
        for validate in [validate_schema_name, validate_stored_schema_name] {
            assert!(validate("").unwrap_err().contains("invalid"));
            validate("application").unwrap();
        }
    }
}

pub mod creation;
