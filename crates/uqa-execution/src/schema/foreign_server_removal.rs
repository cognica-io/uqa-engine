//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind foreign catalog lifetimes in written order before deleting their combined dependency closure.

mod targets;
use targets::ForeignKind;

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
        dependencies::ObjectAddress,
        roles::{guards::RoleCatalogGuards, RoleReferenceNames},
    },
    SQLError,
};
use uqa_storage::{CatalogFacade, StorageBackendError};

pub struct ForeignCatalogRemovalContext<'a> {
    pub publication: ForeignServerRemovalPublication<'a>,
    pub namespace: &'a dyn ForeignCreationNamespace,
    pub locks: &'a dyn SharedObjectLockSession,
    pub writer: &'a dyn RelationDefinitionSession,
    pub roles: &'a dyn RoleCatalogGuards,
    pub session: &'a dyn RoleReferenceNames,
    pub deletion: &'a dyn CatalogRemovalInputs,
    pub notices: &'a crate::query::NoticeQueue,
}

/// Retain the existing server API name while sharing its transaction and object-binding context.
pub type ForeignServerRemovalContext<'a> = ForeignCatalogRemovalContext<'a>;

pub struct ForeignServerRemovalPublication<'a> {
    pub registry: &'a dyn ForeignCreationRegistry,
    pub catalog: Option<&'a dyn CatalogFacade>,
    pub changes: &'a dyn CatalogPublicationChanges,
}

impl ForeignCatalogRemovalContext<'_> {
    pub fn drop_servers(&self, statement: &DropStmt) -> Result<(), SQLError> {
        self.drop_objects(statement, ForeignKind::Server)
    }

    pub fn drop_wrappers(&self, statement: &DropStmt) -> Result<(), SQLError> {
        self.drop_objects(statement, ForeignKind::Wrapper)
    }

    fn drop_objects(&self, statement: &DropStmt, kind: ForeignKind) -> Result<(), SQLError> {
        self.refresh()?;
        let mut originals = Vec::new();
        for name in &statement.names {
            let Some(address) = self.bind(name, statement.if_exists, kind)? else {
                self.notices.push(kind.notice(name));
                continue;
            };
            if !originals.contains(&address) {
                originals.push(address);
            }
        }
        self.delete(originals, statement.cascade)
    }

    /// The direct API returns false for a missing server and uses the same authority, locks and RESTRICT deletion as SQL.
    pub fn drop_server(&self, name: &str) -> Result<bool, SQLError> {
        self.refresh()?;
        let Some(address) = self.bind(name, true, ForeignKind::Server)? else {
            return Ok(false);
        };
        self.delete(vec![address], false)?;
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
        kind: ForeignKind,
    ) -> Result<Option<ObjectAddress>, SQLError> {
        loop {
            let initial = kind.lookup(self, name);
            let Some(initial) = initial else {
                return if if_exists {
                    Ok(None)
                } else {
                    Err(kind.missing(name))
                };
            };
            let guard = self.locks.acquire_shared_catalog(
                SharedCatalogLock::Object {
                    class_id: initial.address().class_id,
                    oid: initial.address().object_id,
                },
                RelationLockMode::AccessExclusive,
            )?;
            self.locks.refresh_shared_catalog()?;
            let current = kind.lookup(self, name);
            let Some(current) = current.filter(|current| {
                current.address() == initial.address()
                    && current.incarnation() == initial.incarnation()
            }) else {
                continue;
            };
            current.ensure_authority(self)?;
            guard.retain();
            return Ok(Some(current.address()));
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
