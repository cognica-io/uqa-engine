//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish sequence role ownership while retaining the authorization catalog guards.
use super::alteration::SequenceDefinitionCatalog;
use crate::catalog::security::{
    roles::{
        dependencies::{prepare_role_owner, RoleDependencyCandidate},
        locking::RoleLockContext,
        RoleCatalogGuards,
    },
    BoundSequenceSecurity,
};
use crate::row_locks::binding::RelationDefinitionSession;
use crate::row_locks::shared_objects::SharedObjectLockSession;
use crate::schema::publication::dependencies::CatalogPublicationChanges;
use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::roles::{self, RoleReferenceNames},
    catalog::security::ownership::{OwnerChangeAuthority, RelationOwnerSchemas},
    schema::sequences::ownership,
    SQLError,
};

pub trait SequenceRoleAccess {
    fn ensure_sequence_owner(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<String, SQLError>;
}
pub trait SequenceSecurityPublication {
    fn security(&self, relation: &RelationIdentity) -> Option<BoundSequenceSecurity>;
    fn persist_security(
        &self,
        name: &str,
        relation: &RelationIdentity,
        security: &BoundSequenceSecurity,
    ) -> Result<(), SQLError>;
    fn publish_security(&self, relation: &RelationIdentity, security: BoundSequenceSecurity);
}
pub struct SequenceRoleOwnershipContext<'a> {
    pub roles: &'a dyn RoleCatalogGuards,
    pub session: &'a dyn RoleReferenceNames,
    pub access: &'a dyn SequenceRoleAccess,
    pub schemas: &'a dyn RelationOwnerSchemas,
    pub locks: &'a dyn SharedObjectLockSession,
    pub writer: &'a dyn RelationDefinitionSession,
    pub metadata: &'a dyn SequenceDefinitionCatalog,
    pub security: &'a dyn SequenceSecurityPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
}
pub fn alter_sequence_role_owner(
    context: &SequenceRoleOwnershipContext<'_>,
    name: &str,
    relation: &RelationIdentity,
    requested_owner: &uqa_sql::ast::RoleSpecification,
) -> Result<(), SQLError> {
    let new_owner = roles::resolve_role_specification(context.session, requested_owner);
    let locks = RoleLockContext {
        roles: context.roles,
        session: context.locks,
    };
    let owner = locks.bind(&new_owner)?;
    let new_owner = owner.name.clone();
    let current_user = context.session.current_role();
    let RoleDependencyCandidate {
        roles,
        memberships,
        value,
        ..
    } = prepare_role_owner(
        locks,
        &owner,
        || context.writer.prepare_definition_write(),
        |roles, memberships| {
            let security = context.security.security(relation).ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no security metadata"))
            })?;
            if security.role_owner == owner.identity() {
                return Ok(None);
            }
            let mut security = security.resolve(roles).map_err(SQLError::Internal)?;
            let state = context
                .metadata
                .state(relation)
                .ok_or_else(|| SQLError::Internal(format!("sequence `{name}` disappeared")))?;
            ownership::reject_owned_sequence_role_change(&relation.name, state.owner.is_some())?;
            let authority = OwnerChangeAuthority {
                roles,
                memberships,
                current_user: &current_user,
                new_owner: &new_owner,
            };
            authority.require_owner_change(&security.role_owner, "sequence", &relation.name)?;
            authority.require_schema_create(context.schemas, &relation.schema)?;
            crate::catalog::security::sequence::rewrite_acl_owner(&mut security, &new_owner);
            Ok(Some(
                BoundSequenceSecurity::bind(&security, roles).map_err(SQLError::Internal)?,
            ))
        },
    )?;
    let Some(security) = value else {
        return Ok(());
    };
    context
        .security
        .persist_security(name, relation, &security)?;
    context.security.publish_security(relation, security);
    drop(memberships);
    drop(roles);
    context.changes.catalog_registry_changed();
    Ok(())
}

pub type OwnedSequenceSecurityRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, BoundSequenceSecurity>> + 'a>;
pub type OwnedSequenceSecurityWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RelationIdentity, BoundSequenceSecurity>> + 'a>;

pub trait OwnedSequenceSecurityCatalog {
    fn owned_sequences(&self, table_object_id: [u8; 16]) -> Vec<RelationIdentity>;
    fn security_registry(&self) -> OwnedSequenceSecurityRead<'_>;
}

pub fn table_owned_sequence_owner_updates(
    catalog: &dyn OwnedSequenceSecurityCatalog,
    table_object_id: [u8; 16],
    new_owner: &str,
    roles: &BTreeMap<String, roles::RoleDefinition>,
) -> Result<Vec<(RelationIdentity, BoundSequenceSecurity)>, SQLError> {
    let owned = catalog.owned_sequences(table_object_id);
    let registry = catalog.security_registry();
    let mut updates = Vec::with_capacity(owned.len());
    for relation in owned {
        let security = registry.get(&relation).cloned().ok_or_else(|| {
            SQLError::Internal(format!(
                "sequence `{}` has no security metadata",
                relation.qualified_name()
            ))
        })?;
        let mut security = security.resolve(roles).map_err(SQLError::Internal)?;
        crate::catalog::security::sequence::rewrite_acl_owner(&mut security, new_owner);
        updates.push((
            relation,
            BoundSequenceSecurity::bind(&security, roles).map_err(SQLError::Internal)?,
        ));
    }
    Ok(updates)
}
