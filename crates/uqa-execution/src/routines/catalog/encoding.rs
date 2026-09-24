//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned routine authority converts legacy names only during initial restoration.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uqa_sql::{
    ast::CreateFunction,
    catalog::roles::RoleDefinition,
    routines::security::binding::{
        bind_legacy_authority, validate_routine_authority, validate_routine_authority_identities,
        LegacyRoutineAclEntry,
    },
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

type Definitions = BTreeMap<String, Vec<CreateFunction>>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutineCatalog<Definitions> {
    routine_catalog_format: u32,
    definitions: Definitions,
}

pub(super) fn encode(definitions: Definitions) -> StorageBackendResult<String> {
    for definition in definitions.values().flatten() {
        validate_routine_authority_identities(definition)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        validate_object_identity(definition)?;
    }
    Ok(serde_json::to_string(&RoutineCatalog {
        routine_catalog_format: 2,
        definitions,
    })?)
}

fn validate_object_identity(definition: &CreateFunction) -> StorageBackendResult<()> {
    if definition
        .object_id
        .is_none_or(|identity| identity == [0; 16])
    {
        return Err(StorageBackendError::Other(format!(
            "routine `{}` has no catalog object identity",
            definition.name
        )));
    }
    Ok(())
}

pub(crate) fn decode(
    json: Option<&str>,
    roles: &BTreeMap<String, RoleDefinition>,
    allows_migration: bool,
) -> StorageBackendResult<(Definitions, bool)> {
    let value: serde_json::Value = json
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_else(|| serde_json::json!({}));
    let (format, mut definitions) = if value.get("routine_catalog_format").is_some() {
        let catalog: RoutineCatalog<BTreeMap<String, Vec<serde_json::Value>>> =
            serde_json::from_value(value)?;
        if !matches!(catalog.routine_catalog_format, 1 | 2) {
            return Err(StorageBackendError::Other(format!(
                "unknown routine catalog format {}",
                catalog.routine_catalog_format
            )));
        }
        (catalog.routine_catalog_format, catalog.definitions)
    } else {
        (0, serde_json::from_value(value)?)
    };
    if format != 2 && !allows_migration {
        return Err(StorageBackendError::Other(
            "routine authority requires initial catalog migration".into(),
        ));
    }
    for definition in definitions.values_mut().flatten() {
        if format == 2 {
            if definition.get("owner").is_none() || definition.get("execute_acl").is_none() {
                return Err(StorageBackendError::Other(
                    "current routine authority fields are missing".into(),
                ));
            }
        } else {
            bind_legacy_definition(definition, roles, format == 0)?;
        }
    }
    let definitions: Definitions = definitions
        .into_iter()
        .map(|(name, overloads)| {
            let overloads = overloads
                .into_iter()
                .map(serde_json::from_value)
                .collect::<Result<Vec<CreateFunction>, _>>()?;
            Ok((name, overloads))
        })
        .collect::<Result<_, serde_json::Error>>()?;
    for definition in definitions.values().flatten() {
        validate_routine_authority(definition, roles)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        if format == 2 {
            validate_object_identity(definition)?;
        }
    }
    Ok((definitions, format != 2))
}

fn bind_legacy_definition(
    definition: &mut serde_json::Value,
    roles: &BTreeMap<String, RoleDefinition>,
    implicit_owner_execute: bool,
) -> StorageBackendResult<()> {
    let object = definition.as_object_mut().ok_or_else(|| {
        StorageBackendError::Other("legacy routine definition is not an object".into())
    })?;
    let owner = object
        .get("owner")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| StorageBackendError::Other("legacy routine owner is missing".into()))?;
    let acl: Option<Vec<LegacyRoutineAclEntry>> = object
        .get("execute_acl")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .flatten();
    let bound = bind_legacy_authority(owner, acl, roles, implicit_owner_execute)
        .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    object.insert("owner".into(), serde_json::to_value(bound.owner)?);
    object.insert(
        "execute_acl".into(),
        serde_json::to_value(bound.execute_acl)?,
    );
    Ok(())
}

#[cfg(test)]
mod tests;
