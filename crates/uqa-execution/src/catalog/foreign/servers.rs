//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-server persistence and initial-open identity migration. Connection options remain separate from catalog metadata.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::catalog::{
    foreign_server::{validate_foreign_servers, ForeignServerDefinition, ForeignServerMetadata},
    roles::{RoleDefinition, RoleIdentity},
};
use uqa_storage::{CatalogFacade, ForeignServerRow, StorageBackendError, StorageBackendResult};

const FORMAT_KEY: &str = "foreign-server-metadata-format";
const FORMAT_MARKER: &str = r#"{"version":1}"#;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerEnvelope {
    format_version: u32,
    metadata: ForeignServerMetadata,
}

pub fn fdw_definition(server: &ForeignServerDefinition) -> uqa_fdw::ForeignServer {
    uqa_fdw::ForeignServer {
        name: server.name.clone(),
        fdw_type: server.fdw_type.clone(),
        options: server.options.clone(),
    }
}

pub fn catalog_row(server: &ForeignServerDefinition) -> StorageBackendResult<ForeignServerRow> {
    Ok(ForeignServerRow {
        name: server.name.clone(),
        fdw_type: server.fdw_type.clone(),
        options_json: serde_json::to_string(&server.options)?,
        metadata_json: Some(serde_json::to_string(&ServerEnvelope {
            format_version: 1,
            metadata: server.metadata.clone(),
        })?),
    })
}

/// Persist inside the caller's metadata transaction, together with its format marker.
pub fn persist(
    catalog: &dyn CatalogFacade,
    server: &ForeignServerDefinition,
) -> StorageBackendResult<()> {
    let current = check_format(catalog)?;
    catalog.save_foreign_server_row(&catalog_row(server)?)?;
    if !current {
        catalog.set_metadata(FORMAT_KEY, FORMAT_MARKER)?;
    }
    Ok(())
}

pub(super) struct RestoredServers {
    pub definitions: BTreeMap<String, ForeignServerDefinition>,
    migrations: Vec<ForeignServerRow>,
    initialize_format: bool,
}

impl RestoredServers {
    /// Called only after all foreign definitions have passed restoration checks.
    pub(super) fn persist_migrations(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for row in &self.migrations {
            catalog.save_foreign_server_row(row)?;
        }
        if self.initialize_format {
            catalog.set_metadata(FORMAT_KEY, FORMAT_MARKER)?;
        }
        Ok(())
    }
}

pub(super) fn restore(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
    allow_migration: bool,
) -> StorageBackendResult<RestoredServers> {
    let current = check_format(catalog)?;
    let rows = catalog.load_foreign_server_rows()?;
    let mut definitions = BTreeMap::new();
    let mut legacy = Vec::new();
    for row in rows {
        let Some(json) = row.metadata_json.as_deref() else {
            if current || !allow_migration {
                return Err(invalid(format!(
                    "foreign server `{}` has no catalog metadata and requires an initial-open migration",
                    row.name
                )));
            }
            legacy.push(row);
            continue;
        };
        if !current {
            return Err(invalid(
                "foreign server metadata exists without its format marker",
            ));
        }
        let envelope: ServerEnvelope = serde_json::from_str(json)?;
        if envelope.format_version != 1 {
            return Err(invalid(format!(
                "unsupported foreign server metadata format {}",
                envelope.format_version
            )));
        }
        insert(&mut definitions, row, envelope.metadata)?;
    }
    validate_foreign_servers(&definitions, roles).map_err(|error| invalid(error.to_string()))?;
    let mut oids = definitions
        .values()
        .map(|s| s.metadata.oid)
        .collect::<BTreeSet<_>>();
    let mut identities = definitions
        .values()
        .map(|s| s.metadata.object_id)
        .collect::<BTreeSet<_>>();
    let mut migrations = Vec::new();
    for row in legacy {
        let oid = loop {
            let candidate = crate::catalog::identity::allocate_catalog_oid("foreign server")
                .map_err(|error| invalid(error.to_string()))?;
            let candidate = u32::try_from(candidate).map_err(|error| invalid(error.to_string()))?;
            if oids.insert(candidate) {
                break candidate;
            }
        };
        let object_id = loop {
            let candidate = crate::catalog::identity::new_nonzero_catalog_identity(
                &row.name,
                "foreign server",
            )?;
            if identities.insert(candidate) {
                break candidate;
            }
        };
        let name = row.name.clone();
        insert(
            &mut definitions,
            row,
            ForeignServerMetadata {
                oid,
                object_id,
                owner: RoleIdentity::BOOTSTRAP,
                server_type: None,
                version: None,
            },
        )?;
        migrations.push(catalog_row(&definitions[&name])?);
    }
    validate_foreign_servers(&definitions, roles).map_err(|error| invalid(error.to_string()))?;
    Ok(RestoredServers {
        definitions,
        migrations,
        initialize_format: !current && allow_migration,
    })
}

fn insert(
    definitions: &mut BTreeMap<String, ForeignServerDefinition>,
    row: ForeignServerRow,
    metadata: ForeignServerMetadata,
) -> StorageBackendResult<()> {
    let definition = ForeignServerDefinition {
        name: row.name.clone(),
        fdw_type: row.fdw_type,
        options: serde_json::from_str(&row.options_json)?,
        metadata,
    };
    if definitions.insert(row.name.clone(), definition).is_some() {
        return Err(invalid(format!("duplicate foreign server `{}`", row.name)));
    }
    Ok(())
}

fn check_format(catalog: &dyn CatalogFacade) -> StorageBackendResult<bool> {
    let Some(marker) = catalog.get_metadata(FORMAT_KEY)? else {
        return Ok(false);
    };
    if serde_json::from_str::<serde_json::Value>(&marker)? != serde_json::json!({"version": 1}) {
        return Err(invalid("unsupported foreign server metadata format marker"));
    }
    Ok(true)
}

fn invalid(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
