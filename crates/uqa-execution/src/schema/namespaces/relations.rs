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
use uqa_sql::ast::RelationPersistence;
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
    fn relation_array_name(&self, relation: &RelationIdentity, array_oid: u32) -> Option<String>;
    fn displace_generated_array(&self, identity: &RelationIdentity) -> Result<bool, SQLError>;
    fn displace_relation_array(&self, identity: &RelationIdentity) -> Result<bool, SQLError>;
    fn synchronize_catalog_registries(&self) -> StorageBackendResult<()>;
    fn synchronize_table_catalog(&self) -> StorageBackendResult<()>;
    fn synchronize_table_data(&self) -> StorageBackendResult<()>;
    fn backend_transaction_is_deferred(&self) -> bool;
    fn fence_catalog_writer_and_refresh_snapshot(&self) -> Result<(), SQLError>;
    /// Create the session's temporary namespace and its TOAST namespace with the counter's next OIDs, which a rollback past this point forgets.
    fn create_temporary_namespace(&self) -> Result<(), SQLError>;
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
        self.access_temporary_namespace()?;
        Ok(RelationIdentity::new(temporary_schema, relation).qualified_name())
    }
    /// `RangeVarGetAndCheckCreationNamespace` for a new relation: its canonical name and the persistence `RangeVarAdjustRelationPersistence` gives it there. A relation that [goes to the session's temporary namespace](Self::targets_temporary_namespace) creates the namespace when it does not exist yet; any other goes to its schema, which must exist and allow CREATE. A relation in the temporary namespace or its TOAST namespace is then temporary, and a temporary relation elsewhere or an unlogged one there is `42P16`.
    pub fn relation_target(
        &self,
        name: &str,
        persistence: RelationPersistence,
    ) -> Result<(String, RelationPersistence), SQLError> {
        let name = if self.targets_temporary_namespace(name, persistence)? {
            self.temporary_name(name)?
        } else {
            self.persistent_relation_name(name)?
        };
        let persistence = self.adjusted_persistence(&name, persistence)?;
        Ok((name, persistence))
    }
    /// `RangeVarAdjustRelationPersistence` for a relation of canonical `name`.
    pub fn adjusted_persistence(
        &self,
        name: &str,
        persistence: RelationPersistence,
    ) -> Result<RelationPersistence, SQLError> {
        let schema = RelationIdentity::from_legacy_name(name)
            .map_err(SQLError::Internal)?
            .schema;
        creation::adjusted_relation_persistence(
            &schema,
            persistence,
            &self.state.temporary_schema_name(),
        )
    }
    /// Whether `RangeVarGetCreationNamespace` puts a new relation in the session's temporary namespace: a temporary relation without a schema, one qualified with `pg_temp` or with the namespace's own name once it exists, and one without a schema whose search path leads with `pg_temp`.
    pub fn targets_temporary_namespace(
        &self,
        name: &str,
        persistence: RelationPersistence,
    ) -> Result<bool, SQLError> {
        let (schema, _) = RelationIdentity::parse_reference(name).map_err(SQLError::Unsupported)?;
        let temporary_schema = self.state.temporary_schema_name();
        match schema.as_deref() {
            Some("pg_temp") => Ok(true),
            Some(schema) => {
                Ok(schema == temporary_schema && self.schemas.temporary_namespace_allocated())
            }
            None if persistence == RelationPersistence::Temporary => Ok(true),
            None => {
                self.runtime
                    .synchronize_catalog_registries()
                    .map_err(|error| {
                        SQLError::Internal(format!("refresh schema catalog: {error}"))
                    })?;
                Ok(creation::relation_creation_schema(
                    self.state,
                    &self.schema_privileges(),
                    &self.names.current_role(),
                )
                .is_some_and(|schema| schema == temporary_schema))
            }
        }
    }
    /// `AccessTempTableNamespace`: the session's first temporary object creates the session's temporary namespace, and later ones find it.
    fn access_temporary_namespace(&self) -> Result<(), SQLError> {
        if self.schemas.temporary_namespace_allocated() {
            return Ok(());
        }
        self.runtime.create_temporary_namespace()
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
    /// Whether a relation of any kind already has this name. This does not reserve the name.
    pub fn relation_name_in_use(&self, relation: &RelationIdentity) -> bool {
        creation::relation_name_in_use(self.relations, relation)
    }
    pub fn reserve_name(&self, name: &str) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        super::relation_names::reserve_relation_name(self.locks, &relation, || {
            Ok(creation::relation_name_in_use(self.relations, &relation))
        })
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
                self.access_temporary_namespace()?;
            }
            target.schema.clone_from(&temporary);
        } else if target.schema == temporary && !self.schemas.temporary_namespace_allocated() {
            return Err(super::locking::missing(&temporary));
        }
        let name = target.qualified_name();
        self.lock_relation_namespace(|| {
            let name = self.resolve_persistent_name(&name)?;
            self.ensure_namespace_create(&target.schema)?;
            Ok(name)
        })?;
        Ok(target)
    }
    /// Check an existing namespace without acquiring a creation dependency. Temporary namespace CREATE follows the current role's database TEMP privilege.
    pub fn ensure_namespace_create(&self, schema: &str) -> Result<(), SQLError> {
        if schema == self.state.temporary_schema_name() {
            return self
                .ensure_temporary_privilege()
                .map_err(|error| match error {
                    SQLError::Routine { sqlstate, .. } if sqlstate == "42501" => {
                        SQLError::Routine {
                            sqlstate,
                            message: format!("permission denied for schema {schema}"),
                        }
                    }
                    error => error,
                });
        }
        self.schema_privileges().require_schema_privilege(
            schema,
            &self.names.current_role(),
            SchemaAclPrivilege::Create,
        )
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
        let schema = self.resolve_creation_schema(schema)?;
        Ok(RelationIdentity::new(schema, relation).qualified_name())
    }
    /// `LookupCreationNamespace` for an explicit schema: the schema must exist and allow CREATE for the current role.
    pub fn creation_namespace(&self, schema: &str) -> Result<String, SQLError> {
        let schema = self.resolve_creation_schema(Some(schema.to_string()))?;
        self.ensure_namespace_create(&schema)?;
        Ok(schema)
    }
    fn resolve_creation_schema(&self, schema: Option<String>) -> Result<String, SQLError> {
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
                return Ok(schema);
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
    fn relation_target(
        &self,
        name: &str,
        persistence: RelationPersistence,
    ) -> Result<(String, RelationPersistence), SQLError> {
        RelationCreationContext::relation_target(self, name, persistence)
    }
}

#[cfg(test)]
mod tests;
