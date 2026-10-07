//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-wrapper catalog identity, separate from server names and source adapters.

use super::roles::{identity::RoleSubject, RoleDefinition, RoleIdentity};
use crate::{ast::FunctionBinding, SQLError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type ForeignWrappers = BTreeMap<String, ForeignWrapperDefinition>;

/// Implementations already provided by the host. A SQL name alone never selects one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeForeignWrapper {
    Memory,
    DuckDB,
    Arrow,
}

impl NativeForeignWrapper {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Memory => "memory_fdw",
            Self::DuckDB => "duckdb_fdw",
            Self::Arrow => "arrow_fdw",
        }
    }

    /// Native wrappers occupy reserved addresses in their own catalog class, below user OIDs.
    pub const fn reference(self) -> ForeignWrapperReference {
        let (oid, tag) = match self {
            Self::Memory => (1, 1),
            Self::DuckDB => (2, 2),
            Self::Arrow => (3, 3),
        };
        let mut object_id = *b"UQA native FDW\0\0";
        object_id[15] = tag;
        ForeignWrapperReference { oid, object_id }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForeignWrapperHandler {
    None,
    Native(NativeForeignWrapper),
    Function(ForeignWrapperFunction),
}

/// Keep `PostgreSQL`'s routine OID even when a validator removes itself during creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignWrapperFunction {
    pub oid: u32,
    pub binding: FunctionBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignWrapperReference {
    pub oid: u32,
    pub object_id: [u8; 16],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignWrapperDefinition {
    pub name: String,
    pub identity: ForeignWrapperReference,
    pub owner: RoleIdentity,
    pub handler: ForeignWrapperHandler,
    pub validator: Option<ForeignWrapperFunction>,
    /// Catalog option arrays retain declaration order, even when execution later needs a map.
    pub options: Vec<(String, String)>,
}

impl ForeignWrapperDefinition {
    pub fn require_handler(&self) -> Result<(), SQLError> {
        if matches!(self.handler, ForeignWrapperHandler::None) {
            return Err(SQLError::Routine {
                sqlstate: "55000".into(),
                message: format!("foreign-data wrapper \"{}\" has no handler", self.name),
            });
        }
        Ok(())
    }
}

pub fn native_wrappers() -> ForeignWrappers {
    [
        NativeForeignWrapper::Memory,
        NativeForeignWrapper::DuckDB,
        NativeForeignWrapper::Arrow,
    ]
    .into_iter()
    .map(|native| {
        let name = native.name().to_owned();
        (
            name.clone(),
            ForeignWrapperDefinition {
                name,
                identity: native.reference(),
                owner: RoleIdentity::BOOTSTRAP,
                handler: ForeignWrapperHandler::Native(native),
                validator: None,
                options: Vec::new(),
            },
        )
    })
    .collect()
}

/// A missing callback is a valid retained reference; a live callback must agree with its saved OID and incarnation.
pub fn validate_functions(
    wrappers: &ForeignWrappers,
    routines: &crate::routines::lifecycle::RoutineRegistry,
) -> Result<(), SQLError> {
    for wrapper in wrappers.values() {
        let handler = match &wrapper.handler {
            ForeignWrapperHandler::Function(function) => Some(function),
            _ => None,
        };
        for reference in handler.into_iter().chain(wrapper.validator.as_ref()) {
            if reference.binding.builtin {
                continue;
            }
            let Some(function) = routines
                .values()
                .flatten()
                .find(|function| function.def.object_id == reference.binding.object_id)
            else {
                continue;
            };
            if function.def.catalog_oid != Some(reference.oid)
                || function.def.is_procedure
                || crate::routines::routine_signature_types(&function.def)
                    != reference.binding.argument_types
            {
                return Err(invalid(
                    &wrapper.name,
                    "live function disagrees with its retained identity",
                ));
            }
        }
    }
    Ok(())
}

/// A stored reference must keep its selected incarnation when a wrapper name is reused.
pub fn bound_wrapper<'a>(
    wrappers: &'a ForeignWrappers,
    name: &str,
    reference: ForeignWrapperReference,
) -> Result<&'a ForeignWrapperDefinition, SQLError> {
    wrappers
        .get(name)
        .filter(|wrapper| wrapper.identity == reference)
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "XX000".into(),
            message: format!(
                "cache lookup failed for foreign-data wrapper {}",
                reference.oid
            ),
        })
}

/// Validate the complete candidate before publishing any restored wrapper.
pub fn validate_wrappers(
    wrappers: &ForeignWrappers,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    let mut oids = BTreeSet::new();
    let mut identities = BTreeSet::new();
    for (name, wrapper) in wrappers {
        if name.is_empty() || name.contains('\0') || name != &wrapper.name {
            return Err(invalid(name, "invalid catalog name"));
        }
        let identity = wrapper.identity;
        let valid_identity = match wrapper.handler {
            ForeignWrapperHandler::Native(native) => identity == native.reference(),
            _ => identity.oid >= super::oids::FIRST_NORMAL_OBJECT_ID,
        };
        if !valid_identity
            || !oids.insert(identity.oid)
            || identity.object_id == [0; 16]
            || !identities.insert(identity.object_id)
        {
            return Err(invalid(name, "invalid or duplicate catalog identity"));
        }
        if !wrapper.owner.is_valid() || wrapper.owner.role_definition(roles).is_none() {
            return Err(invalid(name, "owner references a missing or replaced role"));
        }
        let mut options = BTreeSet::new();
        for (option, _) in &wrapper.options {
            if option.is_empty() || option.contains(['\0', '=']) || !options.insert(option) {
                return Err(invalid(name, "invalid or duplicate option name"));
            }
        }
        let handler = match &wrapper.handler {
            ForeignWrapperHandler::Function(function) => Some(function),
            _ => None,
        };
        for reference in handler.into_iter().chain(wrapper.validator.as_ref()) {
            let function = &reference.binding;
            if reference.oid == 0
                || (!function.builtin && reference.oid < super::oids::FIRST_NORMAL_OBJECT_ID)
                || function.name.is_empty()
                || function.resolution_error.is_some()
                || function.dispatch.is_some()
                || (!function.builtin && function.object_id.is_none_or(|id| id == [0; 16]))
            {
                return Err(invalid(name, "invalid bound function identity"));
            }
        }
    }
    Ok(())
}

fn invalid(name: &str, reason: &str) -> SQLError {
    SQLError::Internal(format!("foreign-data wrapper `{name}`: {reason}"))
}

#[cfg(test)]
mod tests;
