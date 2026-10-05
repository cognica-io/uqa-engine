//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind server lifetimes in written order before deleting their combined dependency closure.

use crate::{
    row_locks::{
        binding::RelationDefinitionSession,
        shared_objects::{SharedCatalogLock, SharedObjectLockSession},
        RelationLockMode,
    },
    schema::{
        deletion::CatalogRemovalInputs,
        foreign_creation::{ForeignCreationNamespace, ForeignCreationRegistry},
        publication::dependencies::CatalogPublicationChanges,
    },
};
use uqa_sql::{
    ast::DropStmt,
    catalog::{
        dependencies::{ObjectAddress, FOREIGN_SERVER_CLASS},
        foreign_server::ForeignServerDefinition,
        roles::{guards::RoleCatalogGuards, RoleReferenceNames},
    },
    schema::foreign_servers::{ensure_drop_authority, missing_server, missing_server_notice},
    SQLError,
};
use uqa_storage::{CatalogFacade, StorageBackendError};

pub struct ForeignServerRemovalContext<'a> {
    pub publication: ForeignServerRemovalPublication<'a>,
    pub namespace: &'a dyn ForeignCreationNamespace,
    pub locks: &'a dyn SharedObjectLockSession,
    pub writer: &'a dyn RelationDefinitionSession,
    pub roles: &'a dyn RoleCatalogGuards,
    pub session: &'a dyn RoleReferenceNames,
    pub deletion: &'a dyn CatalogRemovalInputs,
    pub notices: &'a crate::query::NoticeQueue,
}

pub struct ForeignServerRemovalPublication<'a> {
    pub registry: &'a dyn ForeignCreationRegistry,
    pub catalog: Option<&'a dyn CatalogFacade>,
    pub changes: &'a dyn CatalogPublicationChanges,
}

impl ForeignServerRemovalContext<'_> {
    pub fn drop_servers(&self, statement: &DropStmt) -> Result<(), SQLError> {
        self.refresh()?;
        let mut originals = Vec::new();
        for name in &statement.names {
            let Some(server) = self.bind(name, statement.if_exists)? else {
                self.notices.push(missing_server_notice(name));
                continue;
            };
            let address = ObjectAddress::whole(FOREIGN_SERVER_CLASS, server.metadata.oid);
            if !originals.contains(&address) {
                originals.push(address);
            }
        }
        self.delete(originals, statement.cascade)
    }

    /// The direct API returns false for a missing server and uses the same authority, locks and RESTRICT deletion as SQL.
    pub fn drop_server(&self, name: &str) -> Result<bool, SQLError> {
        self.refresh()?;
        let Some(server) = self.bind(name, true)? else {
            return Ok(false);
        };
        self.delete(
            vec![ObjectAddress::whole(
                FOREIGN_SERVER_CLASS,
                server.metadata.oid,
            )],
            false,
        )?;
        Ok(true)
    }

    fn refresh(&self) -> Result<(), SQLError> {
        self.namespace
            .synchronize_catalog_registries()
            .map_err(storage_error)
    }

    /// `get_object_address` repeats the name lookup after waiting on the original object. Ownership is checked only after that lifetime has been retained.
    fn bind(
        &self,
        name: &str,
        if_exists: bool,
    ) -> Result<Option<ForeignServerDefinition>, SQLError> {
        loop {
            let initial = self.publication.registry.servers().get(name).cloned();
            let Some(initial) = initial else {
                return if if_exists {
                    Ok(None)
                } else {
                    Err(missing_server(name))
                };
            };
            let guard = self.locks.acquire_shared_catalog(
                SharedCatalogLock::Object {
                    class_id: FOREIGN_SERVER_CLASS,
                    oid: initial.metadata.oid,
                },
                RelationLockMode::AccessExclusive,
            )?;
            self.locks.refresh_shared_catalog()?;
            let current = self.publication.registry.servers().get(name).cloned();
            let Some(current) = current.filter(|current| {
                current.metadata.oid == initial.metadata.oid
                    && current.metadata.object_id == initial.metadata.object_id
            }) else {
                continue;
            };
            ensure_drop_authority(&current, self.session, self.roles)?;
            guard.retain();
            return Ok(Some(current));
        }
    }

    fn delete(&self, originals: Vec<ObjectAddress>, cascade: bool) -> Result<(), SQLError> {
        if originals.is_empty() {
            return Ok(());
        }
        self.writer.prepare_definition_write()?;
        crate::schema::deletion::perform_deletion(
            &self.deletion.catalog_removal_context(),
            |_| Ok(originals.clone()),
            cascade,
        )
    }
}

impl ForeignServerRemovalPublication<'_> {
    /// The dependency plan has removed every dependent and retains this server's object lock; do not reauthorize those dependents against the invoking role.
    pub fn remove(&self, name: &str, object_id: [u8; 16]) -> Result<(), SQLError> {
        if self
            .registry
            .servers()
            .get(name)
            .is_none_or(|server| server.metadata.object_id != object_id)
        {
            return Err(SQLError::Internal(format!(
                "foreign server `{name}` changed before catalog removal"
            )));
        }
        if let Some(catalog) = self.catalog {
            catalog.drop_foreign_server(name).map_err(storage_error)?;
        }
        self.registry.servers_write().remove(name);
        self.changes.catalog_registry_changed();
        Ok(())
    }
}

fn storage_error(error: StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error("DROP SERVER", &error)
}
