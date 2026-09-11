//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign server and table creation inside the caller's existing catalog transaction.
use crate::catalog::{
    foreign::StoredForeignTable, security::TableSecurity, services::CatalogSession,
};
use crate::schema::{
    foreign_table_alteration::ForeignTableAlterPublication,
    publication::dependencies::CatalogPublicationChanges,
    sequences::{
        implicit::{self, ImplicitSequenceContext},
        ownership::{self, ImplicitOwnershipContext},
    },
};
use std::{collections::BTreeMap, ops::DerefMut};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::DeferredCreateForeignTable,
    schema::foreign_tables::{validate_foreign_table_schema_envelope, ForeignSchemaContext},
    SQLError,
};
use uqa_storage::{CatalogFacade, StorageBackendResult};

pub mod entry;
pub use crate::catalog::foreign::reads::{
    ForeignRegistryReads, ForeignSecurityRead, ForeignServersRead, ForeignTablesRead,
};
pub type ForeignServersWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<String, uqa_fdw::ForeignServer>> + 'a>;
pub trait ForeignCreationRegistry: ForeignRegistryReads {
    fn servers_write(&self) -> ForeignServersWrite<'_>;
}
pub trait ForeignCreationNamespace {
    fn synchronize_catalog_registries(&self) -> StorageBackendResult<()>;
    fn relation_name_for_create(&self, name: &str) -> Result<String, SQLError>;
    fn relation_kind_at(&self, name: &str) -> StorageBackendResult<Option<&'static str>>;
}
pub struct ForeignCreationContext<'a> {
    pub schema: ForeignSchemaContext<'a>,
    pub namespace: &'a dyn ForeignCreationNamespace,
    pub registry: &'a dyn ForeignCreationRegistry,
    pub publication: &'a dyn ForeignTableAlterPublication,
    pub catalog: Option<&'a dyn CatalogFacade>,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub session: &'a dyn CatalogSession,
    pub sequences: ImplicitSequenceContext<'a>,
    pub ownership: ImplicitOwnershipContext<'a>,
    pub notices: &'a parking_lot::Mutex<Vec<(String, String)>>,
    pub allocate_identity: fn() -> StorageBackendResult<[u8; 16]>,
}
impl ForeignCreationContext<'_> {
    pub fn register_foreign_server_inner(
        &self,
        name: String,
        fdw_type: &str,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> std::result::Result<(), String> {
        self.namespace
            .synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let mut servers = self.registry.servers_write();
        if servers.contains_key(&name) {
            if if_not_exists {
                return Ok(());
            }
            return Err(format!("Foreign server `{name}` already exists"));
        }
        if !matches!(fdw_type, "duckdb_fdw" | "arrow_fdw" | "memory_fdw") {
            return Err(format!("Unsupported FDW type: `{fdw_type}`"));
        }
        let mut opt_map: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (k, v) in options {
            opt_map.insert(k, v);
        }
        let server = uqa_fdw::ForeignServer {
            name: name.clone(),
            fdw_type: fdw_type.to_string(),
            options: opt_map.clone(),
        };
        if let Some(catalog) = self.catalog {
            let options_json = serde_json::to_string(&opt_map)
                .map_err(|err| format!("serialize foreign server `{name}`: {err}"))?;
            catalog
                .save_foreign_server(&name, fdw_type, &options_json)
                .map_err(|err| format!("persist foreign server `{name}`: {err}"))?;
        }
        servers.insert(name, server);
        drop(servers);
        self.changes.catalog_registry_changed();
        Ok(())
    }
    fn preflight_foreign_table_creation(
        &self,
        name: &str,
        if_not_exists: bool,
    ) -> Result<Option<(String, RelationIdentity)>, uqa_sql::SQLError> {
        self.namespace
            .synchronize_catalog_registries()
            .map_err(|error| {
                uqa_sql::SQLError::Internal(format!("refresh FDW catalog: {error}"))
            })?;
        let name = self.namespace.relation_name_for_create(name)?;
        let relation = RelationIdentity::from_legacy_name(&name).map_err(|error| {
            uqa_sql::SQLError::Internal(format!("decode foreign table `{name}`: {error}"))
        })?;
        if self
            .namespace
            .relation_kind_at(&name)
            .map_err(|error| {
                uqa_sql::SQLError::Internal(format!("resolve relation `{name}`: {error}"))
            })?
            .is_some()
        {
            if if_not_exists {
                self.notices.lock().push((
                    "NOTICE".into(),
                    format!("relation \"{}\" already exists, skipping", relation.name),
                ));
                return Ok(None);
            }
            return Err(uqa_sql::SQLError::Routine {
                sqlstate: "42P07".into(),
                message: format!("relation \"{}\" already exists", relation.name),
            });
        }
        {
            let tables = self.registry.tables();
            let table_security = self.registry.security();
            if tables.contains_key(&relation) {
                if !table_security.contains_key(&relation) {
                    return Err(uqa_sql::SQLError::Internal(format!(
                        "foreign table `{name}` has no loaded security metadata"
                    )));
                }
                return Err(uqa_sql::SQLError::Internal(format!(
                    "foreign table `{name}` appeared after relation collision preflight"
                )));
            }
            if table_security.contains_key(&relation) {
                return Err(uqa_sql::SQLError::Internal(format!(
                    "foreign table security metadata exists without table `{name}`"
                )));
            }
        }
        Ok(Some((name, relation)))
    }
    fn ensure_foreign_server_exists(&self, server_name: &str) -> Result<(), uqa_sql::SQLError> {
        if self.registry.servers().contains_key(server_name) {
            return Ok(());
        }
        Err(uqa_sql::SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("server \"{server_name}\" does not exist"),
        })
    }
    pub fn register_foreign_table_inner(
        &self,
        name: &str,
        server_name: String,
        columns: Vec<uqa_sql::ast::ColumnDef>,
        checks: Vec<uqa_sql::ast::TableCheck>,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> Result<(), uqa_sql::SQLError> {
        if !if_not_exists {
            validate_foreign_table_schema_envelope(&columns)?;
        }
        let Some((name, relation)) = self.preflight_foreign_table_creation(name, if_not_exists)?
        else {
            return Ok(());
        };
        if if_not_exists {
            validate_foreign_table_schema_envelope(&columns)?;
        }
        self.register_foreign_table_after_preflight(
            &name,
            relation,
            server_name,
            columns,
            checks,
            options,
        )
    }
    fn register_foreign_table_after_preflight(
        &self,
        name: &str,
        relation: RelationIdentity,
        server_name: String,
        mut columns: Vec<uqa_sql::ast::ColumnDef>,
        mut checks: Vec<uqa_sql::ast::TableCheck>,
        options: Vec<(String, String)>,
    ) -> Result<(), uqa_sql::SQLError> {
        for column in &mut columns {
            column.ty = uqa_sql::type_resolution::resolve_declared_column_type(
                self.schema.types,
                &column.ty,
            )?;
        }
        implicit::materialize_implicit_sequences(
            &self.sequences,
            "CREATE FOREIGN TABLE",
            name,
            &mut columns,
            uqa_sql::ast::RelationPersistence::Permanent,
        )?;
        self.schema
            .prepare_foreign_table_schema(name, &mut columns, &mut checks)?;
        self.ensure_foreign_server_exists(&server_name)?;
        let mut opt_map: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (k, v) in options {
            opt_map.insert(k, v);
        }
        let object_id = (self.allocate_identity)().map_err(|error| {
            uqa_sql::SQLError::Internal(format!(
                "allocate foreign table `{name}` object identity: {error}"
            ))
        })?;
        let owner_columns = columns.clone();
        let table = StoredForeignTable {
            name: name.to_string(),
            object_id,
            server_name,
            columns,
            checks,
            options: opt_map,
        };
        let role_owner = self.session.current_user();
        let security = TableSecurity::owner(role_owner);
        let mut tables = self.publication.tables_write();
        let mut table_security = self.publication.security_write();
        if tables.contains_key(&relation) || table_security.contains_key(&relation) {
            return Err(uqa_sql::SQLError::Internal(format!(
                "foreign table `{name}` changed during creation"
            )));
        }
        if let Some(catalog) = self.catalog {
            catalog
                .save_foreign_table(&table.catalog_row(&relation, &security).map_err(|error| {
                    uqa_sql::SQLError::Internal(format!(
                        "serialize foreign table `{name}`: {error}"
                    ))
                })?)
                .map_err(|error| {
                    uqa_sql::SQLError::Internal(format!("persist foreign table `{name}`: {error}"))
                })?;
        }
        tables.insert(relation.clone(), table);
        table_security.insert(relation, security);
        drop(table_security);
        drop(tables);
        ownership::attach_column_owners(&self.ownership, name, object_id, &owner_columns).map_err(
            |error| {
                uqa_sql::SQLError::Internal(format!(
                    "attach foreign table `{name}` sequence ownership: {error}"
                ))
            },
        )?;
        self.changes.catalog_registry_changed();
        Ok(())
    }
    pub fn drop_foreign_server_inner(&self, name: &str) -> Result<bool, String> {
        self.namespace
            .synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        // Reject when any foreign table references this server.
        let referenced = self
            .registry
            .tables()
            .values()
            .any(|t| t.server_name == name);
        if referenced {
            return Err(format!(
                "foreign server `{name}` is referenced by a foreign table"
            ));
        }
        if !self.registry.servers().contains_key(name) {
            return Ok(false);
        }
        if let Some(catalog) = self.catalog {
            catalog
                .drop_foreign_server(name)
                .map_err(|err| format!("drop foreign server `{name}`: {err}"))?;
        }
        let removed = self.registry.servers_write().remove(name).is_some();
        if removed {
            self.changes.catalog_registry_changed();
        }
        Ok(removed)
    }
    pub fn register_deferred_foreign_table(
        &self,
        deferred: DeferredCreateForeignTable,
    ) -> Result<(), SQLError> {
        let Some((name, relation)) = self.preflight_foreign_table_creation(&deferred.name, true)?
        else {
            return Ok(());
        };
        let statement = uqa_sql::resolve_deferred_create_foreign_table(&deferred)?;
        validate_foreign_table_schema_envelope(&statement.columns)?;
        self.register_foreign_table_after_preflight(
            &name,
            relation,
            statement.server_name,
            statement.columns,
            statement.checks,
            statement.options,
        )
    }
}
