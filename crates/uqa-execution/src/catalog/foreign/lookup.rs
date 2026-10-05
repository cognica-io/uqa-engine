//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign catalog lookup through pinned snapshots or refreshed live registries.
use super::{reads::ForeignRegistryReads, StoredForeignTable};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_storage::StorageBackendResult;
pub trait ForeignLookupState {
    fn query_servers(
        &self,
    ) -> Option<&BTreeMap<String, uqa_sql::catalog::foreign_server::ForeignServerDefinition>>;
    fn query_tables(&self) -> Option<&BTreeMap<RelationIdentity, StoredForeignTable>>;
    fn synchronize_catalog_registries(&self) -> StorageBackendResult<()>;
    fn relation_lookup_candidates(&self, name: &str)
        -> StorageBackendResult<Vec<RelationIdentity>>;
}
pub struct ForeignLookupContext<'a> {
    pub state: &'a dyn ForeignLookupState,
    pub registry: &'a dyn ForeignRegistryReads,
}
impl ForeignLookupContext<'_> {
    pub fn resolve_foreign_table_name(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.state.synchronize_catalog_registries()?;
        let tables = self.registry.tables();
        Ok(self
            .state
            .relation_lookup_candidates(name)?
            .into_iter()
            .find(|candidate| tables.contains_key(candidate))
            .map(|relation| relation.qualified_name()))
    }
    pub fn foreign_server(&self, name: &str) -> Result<Option<uqa_fdw::ForeignServer>, String> {
        if let Some(snapshot) = self.state.query_servers() {
            return Ok(snapshot.get(name).map(super::servers::fdw_definition));
        }
        self.state
            .synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        Ok(self
            .registry
            .servers()
            .get(name)
            .map(super::servers::fdw_definition))
    }
    pub fn foreign_table(&self, name: &str) -> Result<Option<uqa_fdw::ForeignTable>, String> {
        if let Some(snapshot) = self.state.query_tables() {
            return Ok(self
                .state
                .relation_lookup_candidates(name)
                .map_err(|err| format!("resolve foreign table: {err}"))?
                .into_iter()
                .find_map(|relation| {
                    snapshot
                        .get(&relation)
                        .map(StoredForeignTable::fdw_definition)
                }));
        }
        self.state
            .synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let Some(resolved) = self
            .resolve_foreign_table_name(name)
            .map_err(|err| format!("resolve foreign table: {err}"))?
        else {
            return Ok(None);
        };
        let relation = RelationIdentity::from_legacy_name(&resolved)?;
        Ok(self
            .registry
            .tables()
            .get(&relation)
            .map(StoredForeignTable::fdw_definition))
    }
    /// Select the connection from the same retained catalog as its table and validate the captured server identity before invoking a handler.
    pub fn foreign_table_source(
        &self,
        name: &str,
    ) -> Result<(uqa_fdw::ForeignTable, uqa_fdw::ForeignServer), String> {
        if let (Some(tables), Some(servers)) =
            (self.state.query_tables(), self.state.query_servers())
        {
            return self.table_source(name, tables, servers);
        }
        self.state
            .synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        self.table_source(name, &self.registry.tables(), &self.registry.servers())
    }

    fn table_source(
        &self,
        name: &str,
        tables: &BTreeMap<RelationIdentity, StoredForeignTable>,
        servers: &BTreeMap<String, uqa_sql::catalog::foreign_server::ForeignServerDefinition>,
    ) -> Result<(uqa_fdw::ForeignTable, uqa_fdw::ForeignServer), String> {
        let table = self
            .state
            .relation_lookup_candidates(name)
            .map_err(|err| format!("resolve foreign table: {err}"))?
            .into_iter()
            .find_map(|relation| tables.get(&relation))
            .ok_or_else(|| format!("Foreign table `{name}` does not exist"))?;
        let server = table
            .bound_server(servers)
            .map_err(|error| error.to_string())?;
        Ok((
            table.fdw_definition(),
            super::servers::fdw_definition(server),
        ))
    }
    pub fn list_foreign_servers(&self) -> Result<Vec<String>, String> {
        if let Some(snapshot) = self.state.query_servers() {
            let mut out = snapshot.keys().cloned().collect::<Vec<_>>();
            out.sort();
            return Ok(out);
        }
        self.state
            .synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let mut out: Vec<String> = self.registry.servers().keys().cloned().collect();
        out.sort();
        Ok(out)
    }
    pub fn list_foreign_tables(&self) -> Result<Vec<String>, String> {
        if let Some(snapshot) = self.state.query_tables() {
            let mut out = snapshot
                .keys()
                .map(RelationIdentity::qualified_name)
                .collect::<Vec<_>>();
            out.sort();
            return Ok(out);
        }
        self.state
            .synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let mut out: Vec<String> = self
            .registry
            .tables()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect();
        out.sort();
        Ok(out)
    }
    pub fn foreign_table_columns(&self, table: &str) -> Result<Vec<String>, String> {
        let table = self
            .foreign_table(table)?
            .ok_or_else(|| format!("Foreign table `{table}` does not exist"))?;
        Ok(table
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect())
    }
}
