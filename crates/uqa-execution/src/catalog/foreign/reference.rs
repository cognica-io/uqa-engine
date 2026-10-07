//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign tables retain the server they named at creation, including after concurrent deletion or name reuse.

use super::StoredForeignTable;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uqa_sql::{catalog::foreign_server::ForeignServerDefinition, SQLError};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

const FORMAT_KEY: &str = "foreign-table-server-reference-format";
const FORMAT_MARKER: &str = r#"{"version":2}"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignServerReference {
    pub oid: u32,
    pub object_id: [u8; 16],
}

impl From<&ForeignServerDefinition> for ForeignServerReference {
    fn from(server: &ForeignServerDefinition) -> Self {
        Self {
            oid: server.metadata.oid,
            object_id: server.metadata.object_id,
        }
    }
}

impl StoredForeignTable {
    pub fn server_oid(&self) -> Result<u32, SQLError> {
        self.server_reference
            .map(|reference| reference.oid)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "foreign table `{}` has no server identity",
                    self.name
                ))
            })
    }

    pub fn bound_server<'a>(
        &self,
        servers: &'a BTreeMap<String, ForeignServerDefinition>,
    ) -> Result<&'a ForeignServerDefinition, SQLError> {
        let oid = self.server_oid()?;
        servers
            .get(&self.server_name)
            .filter(|server| Some(ForeignServerReference::from(*server)) == self.server_reference)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "XX000".into(),
                message: format!("cache lookup failed for foreign server {oid}"),
            })
    }
}

pub fn validate_query_source(
    catalog: &crate::catalog::CatalogReadView,
    resolution: &crate::catalog::RelationNameResolution,
    name: &str,
) -> Result<(), SQLError> {
    if let Some(table) = catalog.foreign_table_resolved(resolution, name)? {
        let definitions = &catalog.snapshot().definitions;
        table
            .bound_server(&definitions.foreign_servers)?
            .bound_wrapper(&definitions.foreign_wrappers)?
            .require_handler()?;
    }
    Ok(())
}

pub(super) fn validate_schema_reference(
    name: &str,
    version: u8,
    reference: Option<ForeignServerReference>,
) -> StorageBackendResult<()> {
    let valid = match (version, reference) {
        (1, None) => true,
        (2 | 3, Some(reference)) => reference.oid >= 16_384 && reference.object_id != [0; 16],
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(invalid(format!(
            "foreign table `{name}` has invalid server identity for schema version {version}"
        )))
    }
}

pub(crate) fn check_format(catalog: &dyn CatalogFacade) -> StorageBackendResult<Option<u32>> {
    let Some(marker) = catalog.get_metadata(FORMAT_KEY)? else {
        return Ok(None);
    };
    let marker: serde_json::Value = serde_json::from_str(&marker)?;
    for version in [1, 2] {
        if marker == serde_json::json!({"version":version}) {
            return Ok(Some(version));
        }
    }
    Err(invalid(
        "unsupported foreign-table server-reference format marker",
    ))
}

pub(super) fn initialize_format(catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
    catalog.set_metadata(FORMAT_KEY, FORMAT_MARKER)
}

pub(super) fn restore_format(
    catalog: &dyn CatalogFacade,
    allow_migration: bool,
) -> StorageBackendResult<Option<u32>> {
    let version = check_format(catalog)?;
    if version != Some(2) && !allow_migration {
        return Err(invalid(
            "foreign-table option order requires an initial-open migration",
        ));
    }
    Ok(version)
}

pub(super) fn validate_schema_format(
    version: Option<u32>,
    legacy: bool,
    name: &str,
) -> StorageBackendResult<()> {
    if version == Some(2) && legacy {
        return Err(invalid(format!(
            "foreign table `{name}` has a legacy schema under the current option-order marker"
        )));
    }
    Ok(())
}

/// Legacy names bind once, within complete initial restoration. Current references may name a concurrently deleted server and must never bind a replacement.
pub(super) fn restore(
    table: &mut StoredForeignTable,
    servers: &BTreeMap<String, ForeignServerDefinition>,
    current: bool,
    allow_migration: bool,
) -> StorageBackendResult<bool> {
    if table.server_reference.is_some() {
        if !current {
            return Err(invalid(
                "foreign-table server identity exists without its format marker",
            ));
        }
        return Ok(false);
    }
    if current || !allow_migration {
        return Err(invalid(format!(
            "foreign table `{}` has no server identity and requires an initial-open migration",
            table.name
        )));
    }
    let server = servers.get(&table.server_name).ok_or_else(|| {
        invalid(format!(
            "foreign table `{}` references missing server `{}`",
            table.name, table.server_name
        ))
    })?;
    table.server_reference = Some(ForeignServerReference::from(server));
    Ok(true)
}

fn invalid(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
