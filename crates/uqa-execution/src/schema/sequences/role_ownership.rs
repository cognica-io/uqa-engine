//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish sequence role ownership while retaining the authorization catalog guards.
use super::alteration::SequenceDefinitionCatalog;
use crate::catalog::security::{roles::RoleCatalogGuards, SequenceSecurity};
use crate::schema::publication::dependencies::CatalogPublicationChanges;
use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::roles::{self, RoleReferenceNames},
    schema::sequences::{lifecycle::SequenceLifecycleCatalog, ownership},
    SQLError,
};

pub trait SequenceRoleAccess {
    fn ensure_sequence_owner(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<String, SQLError>;
    fn current_user_is_superuser(&self) -> bool;
}
pub trait SequenceSecurityPublication {
    fn security(&self, relation: &RelationIdentity) -> Option<SequenceSecurity>;
    fn persist_security(
        &self,
        name: &str,
        relation: &RelationIdentity,
        security: &SequenceSecurity,
    ) -> Result<(), SQLError>;
    fn publish_security(&self, relation: &RelationIdentity, security: SequenceSecurity);
}
pub struct SequenceRoleOwnershipContext<'a> {
    pub roles: &'a dyn RoleCatalogGuards,
    pub session: &'a dyn RoleReferenceNames,
    pub access: &'a dyn SequenceRoleAccess,
    pub schemas: &'a dyn SequenceLifecycleCatalog,
    pub metadata: &'a dyn SequenceDefinitionCatalog,
    pub security: &'a dyn SequenceSecurityPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
}
pub fn alter_sequence_role_owner(
    context: &SequenceRoleOwnershipContext<'_>,
    name: &str,
    relation: &RelationIdentity,
    requested_owner: &str,
) -> Result<(), SQLError> {
    let current_owner = context.access.ensure_sequence_owner(name, relation)?;
    let new_owner = roles::resolve_role_reference(context.session, requested_owner);
    let roles = context.roles.role_definitions();
    roles::require_role_exists(&roles, &new_owner)?;
    let memberships = context.roles.role_memberships();
    let current_user = context.session.current_user_name();
    roles::require_set_role(&roles, &memberships, &current_user, &new_owner)?;
    let state = context
        .metadata
        .state(relation)
        .ok_or_else(|| SQLError::Internal(format!("sequence `{name}` disappeared")))?;
    ownership::reject_owned_sequence_role_change(&relation.name, state.owner.is_some())?;
    if current_owner == new_owner {
        return Ok(());
    }
    if !context.access.current_user_is_superuser() {
        context
            .schemas
            .require_schema_create(&relation.schema, &new_owner)?;
    }
    let mut security = context
        .security
        .security(relation)
        .ok_or_else(|| SQLError::Internal(format!("sequence `{name}` has no security metadata")))?;
    crate::catalog::security::sequence::rewrite_acl_owner(&mut security, &new_owner);
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
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, SequenceSecurity>> + 'a>;
pub type OwnedSequenceSecurityWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RelationIdentity, SequenceSecurity>> + 'a>;

pub trait OwnedSequenceSecurityCatalog {
    fn owned_sequences(&self, table_object_id: [u8; 16]) -> Vec<RelationIdentity>;
    fn security_registry(&self) -> OwnedSequenceSecurityRead<'_>;
}

pub fn table_owned_sequence_owner_updates(
    catalog: &dyn OwnedSequenceSecurityCatalog,
    table_object_id: [u8; 16],
    new_owner: &str,
) -> Result<Vec<(RelationIdentity, SequenceSecurity)>, SQLError> {
    let owned = catalog.owned_sequences(table_object_id);
    let registry = catalog.security_registry();
    let mut updates = Vec::with_capacity(owned.len());
    for relation in owned {
        let mut security = registry.get(&relation).cloned().ok_or_else(|| {
            SQLError::Internal(format!(
                "sequence `{}` has no security metadata",
                relation.qualified_name()
            ))
        })?;
        crate::catalog::security::sequence::rewrite_acl_owner(&mut security, new_owner);
        updates.push((relation, security));
    }
    Ok(updates)
}
