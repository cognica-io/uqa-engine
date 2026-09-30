//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Privilege names of `GRANT | REVOKE ... ON TYPE | DOMAIN`, validated as `ExecuteGrantStmt` does after the targets and grantees are resolved.

use crate::ast::{TypeObjectKind, TypePrivilege};
use crate::SQLError;

/// Whether the command names `USAGE`, the only type privilege. `ALL` is `USAGE`, `RULE` is ignored, any other privilege is invalid for types, and an unknown name is a syntax error.
pub fn requested_type_usage(
    privileges: &[TypePrivilege],
    kind: TypeObjectKind,
) -> Result<bool, SQLError> {
    let mut usage = false;
    for privilege in privileges {
        let name = match privilege {
            TypePrivilege::Usage => {
                usage = true;
                continue;
            }
            TypePrivilege::Unsupported(name) => name.to_ascii_lowercase(),
        };
        let display = match name.as_str() {
            "rule" => continue,
            "temporary" | "temp" => "TEMPORARY".to_string(),
            "insert" | "select" | "update" | "delete" | "truncate" | "references" | "trigger"
            | "execute" | "create" | "connect" | "set" | "alter system" | "maintain" => {
                name.to_ascii_uppercase()
            }
            _ => {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: format!("unrecognized privilege type \"{name}\""),
                })
            }
        };
        return Err(SQLError::Routine {
            sqlstate: "0LP01".into(),
            message: format!(
                "invalid privilege type {display} for {}",
                match kind {
                    TypeObjectKind::Type => "type",
                    TypeObjectKind::Domain => "domain",
                }
            ),
        });
    }
    Ok(usage)
}

#[cfg(test)]
mod tests {
    use super::requested_type_usage;
    use crate::ast::{TypeObjectKind, TypePrivilege};

    #[test]
    fn privilege_names_follow_string_to_privilege() {
        assert!(requested_type_usage(&[TypePrivilege::Usage], TypeObjectKind::Type).unwrap());
        assert!(!requested_type_usage(
            &[TypePrivilege::Unsupported("RULE".into())],
            TypeObjectKind::Type
        )
        .unwrap());
        let invalid = requested_type_usage(
            &[TypePrivilege::Unsupported("SELECT".into())],
            TypeObjectKind::Domain,
        )
        .unwrap_err();
        assert_eq!(invalid.sqlstate(), Some("0LP01"));
        assert_eq!(
            invalid.to_string(),
            "invalid privilege type SELECT for domain"
        );
        let temporary = requested_type_usage(
            &[TypePrivilege::Unsupported("TEMP".into())],
            TypeObjectKind::Type,
        )
        .unwrap_err();
        assert_eq!(
            temporary.to_string(),
            "invalid privilege type TEMPORARY for type"
        );
        let unknown = requested_type_usage(
            &[TypePrivilege::Unsupported("FOO".into())],
            TypeObjectKind::Type,
        )
        .unwrap_err();
        assert_eq!(unknown.sqlstate(), Some("42601"));
        assert_eq!(unknown.to_string(), "unrecognized privilege type \"foo\"");
    }
}
