//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable wrapper records and initial-only conversion of legacy server references.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uqa_sql::catalog::{
    foreign_server::ForeignServerDefinition,
    foreign_wrapper::{
        native_wrappers, validate_wrappers, ForeignWrapperDefinition, ForeignWrappers,
    },
    roles::RoleDefinition,
};
use uqa_storage::{CatalogFacade, ForeignServerRow, StorageBackendError, StorageBackendResult};

const FORMAT_KEY: &str = "foreign-wrapper-catalog-format";
const RECORD_PREFIX: &str = "foreign-wrapper/";
const FORMAT_MARKER: &str = r#"{"version":1}"#;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WrapperEnvelope {
    version: u32,
    definition: ForeignWrapperDefinition,
}

pub(super) struct RestoredWrappers {
    pub definitions: ForeignWrappers,
    initialize: bool,
    server_migrations: Vec<ForeignServerRow>,
}

impl RestoredWrappers {
    /// Persist only after every foreign definition passes validation, inside the caller's catalog transaction.
    pub(super) fn persist_migrations(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        if self.initialize {
            for definition in self.definitions.values() {
                persist_record(catalog, definition)?;
            }
        }
        for row in &self.server_migrations {
            catalog.save_foreign_server_row(row)?;
        }
        if self.initialize {
            catalog.set_metadata(FORMAT_KEY, FORMAT_MARKER)?;
        }
        Ok(())
    }
}

pub fn persist(
    catalog: &dyn CatalogFacade,
    definition: &ForeignWrapperDefinition,
) -> StorageBackendResult<()> {
    if !current_format(catalog)? {
        return Err(invalid(
            "foreign-wrapper catalog requires initial-open migration",
        ));
    }
    persist_record(catalog, definition)
}

fn persist_record(
    catalog: &dyn CatalogFacade,
    definition: &ForeignWrapperDefinition,
) -> StorageBackendResult<()> {
    let record = serde_json::to_string(&WrapperEnvelope {
        version: 1,
        definition: definition.clone(),
    })?;
    catalog.set_metadata(&format!("{RECORD_PREFIX}{}", definition.name), &record)
}

pub(super) fn restore(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
    servers: &mut BTreeMap<String, ForeignServerDefinition>,
    allow_migration: bool,
) -> StorageBackendResult<RestoredWrappers> {
    let current = current_format(catalog)?;
    let rows = catalog.metadata_with_prefix(RECORD_PREFIX)?;
    if !current && !rows.is_empty() {
        return Err(invalid(
            "foreign-wrapper records exist without their format marker",
        ));
    }
    if !current && !allow_migration {
        return Err(invalid(
            "foreign-wrapper catalog requires initial-open migration",
        ));
    }
    let mut definitions = if current {
        BTreeMap::new()
    } else {
        native_wrappers()
    };
    for (key, value) in rows {
        let record: WrapperEnvelope = serde_json::from_str(&value)?;
        if record.version != 1 || key != format!("{RECORD_PREFIX}{}", record.definition.name) {
            return Err(invalid("invalid foreign-wrapper record version or key"));
        }
        if definitions
            .insert(record.definition.name.clone(), record.definition)
            .is_some()
        {
            return Err(invalid("duplicate foreign-wrapper catalog record"));
        }
    }
    validate_wrappers(&definitions, roles).map_err(|error| invalid(error.to_string()))?;
    let mut server_migrations = Vec::new();
    for server in servers.values_mut() {
        match server.metadata.wrapper_reference {
            Some(_) if !current => {
                return Err(invalid(
                    "foreign-wrapper reference exists without its format marker",
                ));
            }
            Some(_) => {
                server
                    .bound_wrapper(&definitions)
                    .map_err(|error| invalid(error.to_string()))?;
            }
            None if current || !allow_migration => {
                return Err(invalid(format!(
                    "foreign server `{}` has no wrapper identity and requires an initial-open migration",
                    server.name
                )));
            }
            None => {
                let wrapper = definitions.get(&server.fdw_type).ok_or_else(|| {
                    invalid(format!(
                        "foreign server `{}` references missing wrapper `{}`",
                        server.name, server.fdw_type
                    ))
                })?;
                server.metadata.wrapper_reference = Some(wrapper.identity);
                server_migrations.push(super::servers::catalog_row(server)?);
            }
        }
    }
    Ok(RestoredWrappers {
        definitions,
        initialize: !current,
        server_migrations,
    })
}

fn current_format(catalog: &dyn CatalogFacade) -> StorageBackendResult<bool> {
    let Some(marker) = catalog.get_metadata(FORMAT_KEY)? else {
        return Ok(false);
    };
    if serde_json::from_str::<serde_json::Value>(&marker)? != serde_json::json!({"version":1}) {
        return Err(invalid("unsupported foreign-wrapper catalog format marker"));
    }
    Ok(true)
}

fn invalid(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
