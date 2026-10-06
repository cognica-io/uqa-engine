//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered namespace expansion and declaration checks for routine privileges.

use crate::{
    ast::{AlterRoutineKind, GrantRoutineStmt, RoutinePrivilege},
    catalog::roles::{identity::RoleSubject, RoleDefinition},
    routines::{
        lifecycle::{names::RoutineNameCatalog, RoutineRegistry},
        SQLUserFunction,
    },
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::{catalog_acl::AclGrantee, RelationIdentity};

/// `objectsInSchemaToOids` includes aggregate and window functions in FUNCTIONS, but excludes procedures.
pub fn kind_matches(kind: AlterRoutineKind, prokind: char) -> bool {
    match kind {
        AlterRoutineKind::Function => matches!(prokind, 'f' | 'a' | 'w'),
        AlterRoutineKind::Procedure => prokind == 'p',
        AlterRoutineKind::Routine => matches!(prokind, 'f' | 'a' | 'w' | 'p'),
    }
}

/// Resolve schemas in written order, checking USAGE before inspecting their routines. Repeated schemas contribute repeated targets, including their warnings.
pub fn routines_in_schemas(
    catalog: &dyn RoutineNameCatalog,
    registry: &RoutineRegistry,
    schemas: &[String],
    kind: AlterRoutineKind,
    catalog_oid: impl Fn(&SQLUserFunction) -> Result<i64, SQLError>,
) -> Result<Vec<(String, usize)>, SQLError> {
    let mut targets = Vec::new();
    for schema in schemas {
        let security = catalog
            .schema_security(schema)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "3F000".into(),
                message: format!("schema \"{schema}\" does not exist"),
            })?;
        catalog.require_schema_usage(schema, &catalog.current_role())?;
        let namespace_oid = security.namespace_oid(schema);
        let mut matches = Vec::new();
        for (name, overloads) in registry {
            let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
                SQLError::Internal(format!("invalid routine catalog name `{name}`: {error}"))
            })?;
            if catalog
                .schema_security(&relation.schema)
                .is_none_or(|security| security.namespace_oid(&relation.schema) != namespace_oid)
            {
                continue;
            }
            for (position, routine) in overloads.iter().enumerate() {
                let prokind = if routine.def.is_procedure { 'p' } else { 'f' };
                if kind_matches(kind, prokind) {
                    matches.push((catalog_oid(routine)?, name.clone(), position));
                }
            }
        }
        matches.sort_by_key(|(oid, _, _)| *oid);
        targets.extend(
            matches
                .into_iter()
                .map(|(_, name, position)| (name, position)),
        );
    }
    Ok(targets)
}

/// `GRANTED BY` is checked before the command resolves its target objects or recipients.
pub fn validate_grantor(
    requested: Option<&str>,
    current: &(impl RoleSubject + ?Sized),
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    let Some(requested) = requested else {
        return Ok(());
    };
    if !roles.contains_key(requested) {
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("role \"{requested}\" does not exist"),
        });
    }
    if current.role_name(roles) != Some(requested) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "grantor must be current user".into(),
        });
    }
    Ok(())
}

/// Privilege validation follows target and recipient binding, even for an empty schema.
pub fn validate_privileges(stmt: &GrantRoutineStmt) -> Result<(), SQLError> {
    for privilege in &stmt.privileges {
        let message = match privilege {
            RoutinePrivilege::Execute => continue,
            RoutinePrivilege::Unsupported(name) => {
                let privilege = match name.as_str() {
                    "temporary" | "temp" => "TEMP".into(),
                    "insert" | "select" | "update" | "delete" | "truncate" | "references"
                    | "trigger" | "usage" | "create" | "connect" | "set" | "alter system"
                    | "maintain" => name.to_ascii_uppercase(),
                    _ => {
                        return Err(SQLError::Routine {
                            sqlstate: "42601".into(),
                            message: format!("unrecognized privilege type \"{name}\""),
                        })
                    }
                };
                let kind = crate::routines::lifecycle::alter_routine_kind_name(stmt.kind);
                format!("invalid privilege type {privilege} for {kind}")
            }
            RoutinePrivilege::ColumnsUnsupported => {
                "column privileges are only valid for relations".into()
            }
        };
        return Err(SQLError::Routine {
            sqlstate: "0LP01".into(),
            message,
        });
    }
    Ok(())
}

/// An empty target list never invokes `merge_acl_with_grant`, so a PUBLIC grant option is only rejected when at least one object is selected.
pub fn validate_target_options(
    stmt: &GrantRoutineStmt,
    grantees: &[AclGrantee],
) -> Result<(), SQLError> {
    if stmt.is_grant && stmt.grant_option && grantees.iter().any(AclGrantee::is_public) {
        return Err(SQLError::Routine {
            sqlstate: "0LP01".into(),
            message: "grant options can only be granted to roles".into(),
        });
    }
    Ok(())
}

pub mod targets;

#[cfg(test)]
mod tests;
