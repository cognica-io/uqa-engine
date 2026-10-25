//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shell declarations reserve the ordinary type namespace and one durable type identity, without a value layout or generated array.

use super::namespaces::{
    relations::RelationCreationContext, NamespaceCatalogChanges, SchemaStatementWriter,
};
use crate::catalog::{
    identity::{allocate_catalog_object_id, CatalogIdentityReservationContext},
    shell_type::{self, ShellRegistryPublication},
};
use uqa_sql::{
    catalog::{roles::memberships::role_is_superuser, shell_type::StoredShellType},
    schema::constraint_metadata::{CatalogObjectAllocator, CatalogOidClass},
    SQLError,
};

/// Existing shells are completed under their type-object lock. Lookup and owner checks use current catalog generations after any wait.
pub struct ShellCompletionContext<'a> {
    pub publication: &'a dyn ShellRegistryPublication,
    pub locks: &'a dyn crate::row_locks::shared_objects::SharedObjectLockSession,
}

impl ShellCompletionContext<'_> {
    pub fn retain(
        &self,
        name: &str,
        owner: uqa_sql::catalog::roles::RoleIdentity,
    ) -> Result<Option<StoredShellType>, SQLError> {
        use crate::row_locks::{shared_objects::SharedCatalogLock, RelationLockMode};
        loop {
            let Some(initial) = self.publication.shell_registry().get(name).cloned() else {
                return Ok(None);
            };
            let guard = self.locks.acquire_shared_catalog(
                SharedCatalogLock::Object {
                    class_id: CatalogOidClass::Type.class_id(),
                    oid: initial.oid,
                },
                RelationLockMode::AccessExclusive,
            )?;
            self.locks.refresh_shared_catalog()?;
            let current = self.publication.shell_registry().get(name).cloned();
            if let Some(current) = current.filter(|current| current.object_id == initial.object_id)
            {
                // TypeCreate requires the completing role itself, not inherited or superuser ownership privileges.
                if current.owner != owner {
                    return Err(SQLError::Routine {
                        sqlstate: "42501".into(),
                        message: format!("must be owner of type {}", current.identity.name),
                    });
                }
                guard.retain();
                return Ok(Some(current));
            }
        }
    }

    /// Removal of the shell record and publication of its completed definition share the caller's statement transaction and rollback snapshot.
    pub fn complete(&self, shell: Option<&StoredShellType>) -> Result<(), SQLError> {
        let Some(shell) = shell else { return Ok(()) };
        let before = self.publication.shell_registry().clone();
        let mut registry = before.clone();
        let name = shell.identity.qualified_name();
        if registry
            .get(&name)
            .is_none_or(|current| current.object_id != shell.object_id)
        {
            return Err(SQLError::Routine {
                sqlstate: "40001".into(),
                message: format!("shell type `{name}` changed during completion"),
            });
        }
        registry.remove(&name);
        shell_type::publish(self.publication, &before, registry)
    }
}

pub struct ShellTypeContext<'a> {
    pub registries: super::types::arrays::UserTypeRegistries<'a>,
    pub creation: RelationCreationContext<'a>,
    pub identities: CatalogIdentityReservationContext<'a>,
    pub writer: &'a dyn SchemaStatementWriter,
    pub allocate_identity: fn() -> Result<[u8; 16], SQLError>,
    pub publication: &'a dyn ShellRegistryPublication,
    pub changes: &'a dyn NamespaceCatalogChanges,
}

pub fn create_shell(context: &ShellTypeContext<'_>, name: &str) -> Result<(), SQLError> {
    let owner = context.creation.bind_owner()?;
    // DefineType checks superuser authority before resolving the target namespace, even for an undefined type.
    if !role_is_superuser(&context.creation.roles.role_definitions(), &owner) {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: "must be superuser to create a base type".into(),
        });
    }
    context.writer.prepare_writer()?;
    let name = context.creation.object_name(name)?;
    let requested =
        uqa_core::RelationIdentity::from_legacy_name(&name).map_err(SQLError::Internal)?;
    super::types::arrays::displace_array_type(&context.creation, context.registries, &requested)?;
    let identity = context.creation.reserve_type_name(&name)?;
    let object_id = (context.allocate_identity)()?;
    let oid = context
        .identities
        .allocator(allocate_catalog_object_id)
        .allocate_catalog_oid(CatalogOidClass::Type, &object_id)
        .map_err(|error| {
            uqa_sql::catalog::errors::storage_error("shell catalog identity", &error)
        })?;
    let oid =
        u32::try_from(oid).map_err(|_| SQLError::Internal(format!("invalid shell OID {oid}")))?;
    context.creation.retain_owner(&owner)?;
    let before = context.publication.shell_registry().clone();
    let mut registry = before.clone();
    registry.insert(
        identity.qualified_name(),
        StoredShellType {
            object_id,
            oid,
            identity,
            owner: owner.identity(),
            usage_acl: None,
        },
    );
    shell_type::publish(context.publication, &before, registry)?;
    context.changes.catalog_registry_changed();
    Ok(())
}
