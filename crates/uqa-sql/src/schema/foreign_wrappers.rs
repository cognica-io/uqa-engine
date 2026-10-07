//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` wrapper declaration checks over exact catalog routine identities.

use crate::{
    ast::{ForeignWrapperFunctionOption, FunctionBinding},
    catalog::roles::{memberships::role_is_superuser, RoleDefinition, RoleReference},
    SQLError,
};
use std::collections::BTreeMap;

#[derive(Clone)]
pub struct ForeignWrapperRoutine {
    pub binding: FunctionBinding,
    pub oid: u32,
    pub return_oid: i64,
}

#[derive(Default)]
pub struct ForeignWrapperFunctions {
    pub handler: Option<ForeignWrapperRoutine>,
    pub validator: Option<ForeignWrapperRoutine>,
}

pub fn ensure_create_authority(
    name: &str,
    role: &RoleReference,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    if role_is_superuser(roles, role) {
        return Ok(());
    }
    Err(SQLError::Diagnostic {
        sqlstate: "42501".into(),
        message: format!("permission denied to create foreign-data wrapper \"{name}\""),
        detail: None,
        hint: Some("Must be superuser to create a foreign-data wrapper.".into()),
    })
}

pub fn duplicate_wrapper(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42710".into(),
        message: format!("foreign-data wrapper \"{name}\" already exists"),
    }
}

pub fn ensure_wrapper_usage(
    wrapper: &crate::catalog::foreign_wrapper::ForeignWrapperDefinition,
    role: &RoleReference,
    catalog: &dyn crate::catalog::roles::guards::RoleCatalogGuards,
) -> Result<(), SQLError> {
    // Host-provided native wrappers retain their existing public registration contract.
    if matches!(
        wrapper.handler,
        crate::catalog::foreign_wrapper::ForeignWrapperHandler::Native(_)
    ) || crate::catalog::roles::role_inherits(
        &catalog.role_definitions(),
        &catalog.role_memberships(),
        role,
        &wrapper.owner,
    ) {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "42501".into(),
        message: format!(
            "permission denied for foreign-data wrapper {}",
            wrapper.name
        ),
    })
}

/// Keep each function lookup at its written position, including before a later duplicate clause.
pub fn bind_functions(
    options: &[ForeignWrapperFunctionOption],
    mut lookup: impl FnMut(&str, &[i64], &[String]) -> Result<ForeignWrapperRoutine, SQLError>,
) -> Result<ForeignWrapperFunctions, SQLError> {
    let mut result = ForeignWrapperFunctions::default();
    let (mut handler_given, mut validator_given) = (false, false);
    for option in options {
        let (given, name, handler) = match option {
            ForeignWrapperFunctionOption::Handler(name) => (&mut handler_given, name, true),
            ForeignWrapperFunctionOption::Validator(name) => (&mut validator_given, name, false),
        };
        if *given {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "conflicting or redundant options".into(),
            });
        }
        *given = true;
        let Some(parts) = name else {
            continue;
        };
        let name = function_name(parts)?;
        if handler {
            let function = lookup(&name, &[], &[])?;
            if function.return_oid != 3115 {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("function {} must return type fdw_handler", parts.join(".")),
                });
            }
            result.handler = Some(function);
        } else {
            result.validator = Some(lookup(
                &name,
                &[1009, 26],
                &["text[]".into(), "oid".into()],
            )?);
        }
    }
    Ok(result)
}

fn function_name(parts: &[String]) -> Result<String, SQLError> {
    let local = match parts {
        [_] | [_, _] => parts,
        [database, _, _] if database == crate::catalog::DATABASE_NAME => &parts[1..],
        [_, _, _] => {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: format!(
                    "cross-database references are not implemented: {}",
                    parts.join(".")
                ),
            })
        }
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: format!(
                    "improper qualified name (too many dotted names): {}",
                    parts.join(".")
                ),
            })
        }
    };
    Ok(local
        .iter()
        .map(|part| crate::compiler::render_relation_component(part))
        .collect::<Vec<_>>()
        .join("."))
}

pub fn creation_options(options: &[(String, String)]) -> Result<Vec<(String, String)>, SQLError> {
    super::foreign_servers::creation_options(options)?;
    Ok(options.to_vec())
}

#[cfg(test)]
mod tests;
