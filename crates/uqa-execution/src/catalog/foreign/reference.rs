//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign tables retain the server they named at creation, including after concurrent deletion or name reuse.

use super::StoredForeignTable;
mod input;
pub(crate) use input::check_format_with_control;
use serde::{Deserialize, Serialize};
use uqa_core::memory::{ProductionControl, ProductionMap};
use uqa_sql::{
    catalog::foreign_server::{ForeignServerDefinition, ForeignServers},
    SQLError,
};
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
    fn selects_server(&self, server: &ForeignServerDefinition) -> bool {
        self.server_name == server.name
            && self.server_reference == Some(ForeignServerReference::from(server))
    }

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
        servers: &'a impl uqa_sql::catalog::named_definitions::NamedDefinitionView<
            ForeignServerDefinition,
        >,
    ) -> Result<&'a ForeignServerDefinition, SQLError> {
        let oid = self.server_oid()?;
        servers
            .get(&self.server_name)
            .filter(|server| self.selects_server(server))
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "XX000".into(),
                message: format!("cache lookup failed for foreign server {oid}"),
            })
    }
}

/// A deleted server remains a valid historical binding. When its incarnation is live, its recorded name and OID must select that same server.
pub(crate) fn validate_live_servers<'a>(
    tables: impl Iterator<Item = &'a StoredForeignTable>,
    servers: &ForeignServers,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let mut unresolved = ProductionMap::new(*control);
    for table in tables {
        control.check()?;
        let Some(reference) = table.server_reference else {
            continue;
        };
        match servers.get(&table.server_name) {
            Some(server) if reference.object_id == server.metadata.object_id => {
                if !table.selects_server(server) {
                    return inconsistent_server(&table.name, server, control);
                }
            }
            _ => {
                unresolved.insert_copy(reference.object_id, table.name.as_str())?;
            }
        }
    }
    if !unresolved.is_empty() {
        for server in servers.values() {
            control.check()?;
            if let Some(table) = unresolved.get(&server.metadata.object_id) {
                return inconsistent_server(table, server, control);
            }
        }
    }
    Ok(())
}

fn inconsistent_server(
    table: &str,
    server: &ForeignServerDefinition,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let message = control.format(format_args!(
        "foreign table `{table}` has an inconsistent binding to live foreign server `{}`",
        server.name
    ))?;
    Err(SQLError::Internal(message.into_parts().0))
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
    servers: &impl uqa_sql::catalog::named_definitions::NamedDefinitionView<ForeignServerDefinition>,
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
