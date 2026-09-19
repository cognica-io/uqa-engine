//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schedule creation namespace resolution and writer retries without owning session state.

use crate::catalog::security::roles::{
    dependencies::retain_created_owner,
    locking::{RoleBinding, RoleLockContext},
};
use crate::row_locks::{shared_objects::SharedObjectLockSession, RelationLockMode};
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::{
        resolution::{
            candidates::RelationCandidateState,
            creation::{self, CreationRelationGuards},
        },
        roles::{guards::RoleCatalogGuards, RoleReferenceNames},
        security::{
            database::DatabaseAclPrivilege,
            database_inquiry::{DatabasePrivilegeCatalog, DatabasePrivilegeInquiry},
            schema::SchemaAclPrivilege,
            schema_inquiry::{SchemaPrivilegeCatalog, SchemaPrivilegeInquiry},
        },
    },
    SQLError,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub trait RelationCreationRuntime {
    fn synchronize_catalog_registries(&self) -> StorageBackendResult<()>;
    fn synchronize_table_catalog(&self) -> StorageBackendResult<()>;
    fn synchronize_table_data(&self) -> StorageBackendResult<()>;
    fn backend_transaction_is_deferred(&self) -> bool;
    fn fence_catalog_writer_and_refresh_snapshot(&self) -> Result<(), SQLError>;
    fn allocate_temporary_namespace(&self);
}

#[derive(Clone, Copy)]
pub struct RelationCreationContext<'a> {
    pub names: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub locks: &'a dyn SharedObjectLockSession,
    pub schemas: &'a dyn SchemaPrivilegeCatalog,
    pub database: &'a dyn DatabasePrivilegeCatalog,
    pub state: &'a dyn RelationCandidateState,
    pub relations: &'a dyn CreationRelationGuards,
    pub runtime: &'a dyn RelationCreationRuntime,
}

impl RelationCreationContext<'_> {
    fn role_locks(&self) -> RoleLockContext<'_> {
        RoleLockContext {
            roles: self.roles,
            session: self.locks,
        }
    }
    pub fn bind_owner(&self) -> Result<RoleBinding, SQLError> {
        self.role_locks().bind(&self.names.current_role())
    }
    pub fn retain_owner(&self, owner: &RoleBinding) -> Result<(), SQLError> {
        retain_created_owner(self.role_locks(), owner)
    }
    fn schema_privileges(&self) -> SchemaPrivilegeInquiry<'_> {
        SchemaPrivilegeInquiry {
            catalog: self.schemas,
            names: self.names,
            roles: self.roles,
        }
    }
    pub fn ensure_temporary_privilege(&self) -> Result<(), SQLError> {
        let current_user = self.names.current_role();
        DatabasePrivilegeInquiry {
            catalog: self.database,
            names: self.names,
            roles: self.roles,
        }
        .ensure_database_privilege(&current_user, DatabaseAclPrivilege::Temporary)
    }
    pub fn temporary_name(&self, name: &str) -> Result<String, SQLError> {
        self.ensure_temporary_privilege()?;
        let (temporary_schema, relation) = creation::temporary_creation_parts(self.state, name)?;
        self.runtime.allocate_temporary_namespace();
        Ok(RelationIdentity::new(temporary_schema, relation).qualified_name())
    }
    pub fn api_name(&self, name: &str) -> StorageBackendResult<String> {
        self.lock_relation_namespace(|| {
            self.runtime
                .synchronize_catalog_registries()
                .map_err(|error| SQLError::Internal(format!("refresh schema catalog: {error}")))?;
            creation::api_relation_name(self.state, self.schemas, name).map_err(SQLError::Internal)
        })
        .map_err(|error| match error {
            SQLError::Internal(message) => StorageBackendError::Other(message),
            error => StorageBackendError::backend("CREATE TABLE namespace", error),
        })
    }
    pub fn persistent_relation_name(&self, name: &str) -> Result<String, SQLError> {
        self.lock_relation_namespace(|| self.persistent_name(name))
    }
    /// Resolve an explicit SET SCHEMA destination, including temporary namespace allocation and authority, before the caller checks whether that namespace permits relocation.
    pub fn relocation_target(
        &self,
        target: &RelationIdentity,
    ) -> Result<RelationIdentity, SQLError> {
        let temporary = self.state.temporary_schema_name();
        let mut target = target.clone();
        if target.schema == "pg_temp" {
            if !self.schemas.temporary_namespace_allocated() {
                self.ensure_temporary_privilege()?;
                self.runtime.allocate_temporary_namespace();
            }
            target.schema.clone_from(&temporary);
        } else if target.schema == temporary && !self.schemas.temporary_namespace_allocated() {
            return Err(super::locking::missing(&temporary));
        }
        let name = target.qualified_name();
        self.lock_relation_namespace(|| {
            let name = self.persistent_name(&name)?;
            if target.schema == temporary {
                self.ensure_temporary_privilege()
                    .map_err(|error| match error {
                        SQLError::Routine { sqlstate, .. } if sqlstate == "42501" => {
                            SQLError::Routine {
                                sqlstate,
                                message: format!("permission denied for schema {temporary}"),
                            }
                        }
                        error => error,
                    })?;
            }
            Ok(name)
        })?;
        Ok(target)
    }
    fn lock_relation_namespace(
        &self,
        mut resolve: impl FnMut() -> Result<String, SQLError>,
    ) -> Result<String, SQLError> {
        super::locking::bind_namespace_lifetime(self.locks, RelationLockMode::AccessShare, || {
            let name = resolve()?;
            let relation =
                RelationIdentity::from_legacy_name(&name).map_err(SQLError::Unsupported)?;
            let security = self
                .schema_privileges()
                .schema_security_for_privilege(&relation.schema)
                .ok_or_else(|| super::locking::missing(&relation.schema))?;
            Ok(Some((super::locking::tuple(&security)?, name)))
        })?
        .ok_or_else(|| SQLError::Internal("creation namespace lookup returned no target".into()))
    }
    /// Functions and types resolve authority without retaining relation-creation namespace locks.
    pub fn persistent_name(&self, name: &str) -> Result<String, SQLError> {
        let name = self.resolve_persistent_name(name)?;
        self.ensure_create(&name)?;
        Ok(name)
    }
    pub fn resolve_persistent_name(&self, name: &str) -> Result<String, SQLError> {
        let (schema, relation) =
            RelationIdentity::parse_reference(name).map_err(SQLError::Unsupported)?;
        let current_user = self.names.current_role();
        for attempt in 0..2 {
            self.runtime
                .synchronize_catalog_registries()
                .map_err(|error| SQLError::Internal(format!("refresh schema catalog: {error}")))?;
            let resolved = creation::sql_creation_schema(
                self.state,
                &self.schema_privileges(),
                schema.as_deref(),
                &current_user,
            );
            if let Some(schema) = resolved {
                return Ok(RelationIdentity::new(schema, relation).qualified_name());
            }
            if attempt == 0 && self.runtime.backend_transaction_is_deferred() {
                self.runtime.fence_catalog_writer_and_refresh_snapshot()?;
                continue;
            }
            break;
        }
        Err(creation::missing_creation_schema(schema))
    }
    pub fn ensure_create(&self, canonical_name: &str) -> Result<(), SQLError> {
        creation::ensure_creation_privilege(self.names, &self.schema_privileges(), canonical_name)
    }
    pub fn ensure_existing_create(&self, canonical_name: &str) -> Result<(), SQLError> {
        let relation =
            RelationIdentity::from_legacy_name(canonical_name).map_err(SQLError::Unsupported)?;
        if relation.schema == self.state.temporary_schema_name() {
            return self.ensure_temporary_privilege();
        }
        let current_user = self.names.current_role();
        self.schema_privileges().require_schema_privilege(
            &relation.schema,
            &current_user,
            SchemaAclPrivilege::Usage,
        )?;
        self.schema_privileges().require_schema_privilege(
            &relation.schema,
            &current_user,
            SchemaAclPrivilege::Create,
        )
    }
    pub fn resolve_index_table(&self, name: &str) -> Result<Option<String>, SQLError> {
        self.runtime
            .synchronize_table_catalog()
            .map_err(|error| SQLError::Internal(format!("load table catalog: {error}")))?;
        self.runtime
            .synchronize_table_data()
            .map_err(|error| SQLError::Internal(format!("load table data: {error}")))?;
        self.runtime
            .synchronize_catalog_registries()
            .map_err(|error| SQLError::Internal(format!("load schema catalog: {error}")))?;
        creation::resolve_index_table_name(
            self.names,
            self.state,
            &self.schema_privileges(),
            self.relations,
            name,
        )
    }
}

impl uqa_sql::schema::view_creation::ViewCreationNamespace for RelationCreationContext<'_> {
    fn temporary_schema_name(&self) -> String {
        self.state.temporary_schema_name()
    }
    fn temporary_target(&self, name: &str) -> Result<String, SQLError> {
        self.temporary_name(name)
    }
    fn persistent_target(&self, name: &str) -> Result<String, SQLError> {
        self.persistent_relation_name(name)
    }
}

#[cfg(test)]
mod tests;
