//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain declaration scheduling and publication through current namespace and binding inputs.

use super::namespaces::SchemaStatementWriter;
use crate::catalog::domain::{self, DomainRegistryPublication};
use uqa_sql::{
    ast::CreateDomain,
    catalog::domain::{domain_object_oid, StoredDomain},
    SQLError,
};

pub trait DomainDeclarationBinding {
    fn bind_domain_declaration(&self, definition: &mut CreateDomain) -> Result<(), SQLError>;
}
pub struct DomainCreationContext<'a> {
    pub creation: crate::schema::namespaces::relations::RelationCreationContext<'a>,
    pub identities: crate::catalog::identity::CatalogIdentityReservationContext<'a>,
    pub writer: &'a dyn SchemaStatementWriter,
    pub bindings: &'a dyn DomainDeclarationBinding,
    pub allocate_identity: fn() -> Result<[u8; 16], SQLError>,
    pub publication: &'a dyn DomainRegistryPublication,
    pub changes: &'a dyn super::namespaces::NamespaceCatalogChanges,
}

pub fn create_domain(
    context: &DomainCreationContext<'_>,
    mut definition: CreateDomain,
) -> Result<(), SQLError> {
    let owner = context.creation.bind_owner()?;
    context.writer.prepare_writer()?;
    definition.name = context.creation.persistent_name(&definition.name)?;
    let identity = context.creation.reserve_type_name(&definition.name)?;
    context.bindings.bind_domain_declaration(&mut definition)?;
    let mut allocator = context
        .identities
        .allocator(crate::catalog::identity::allocate_catalog_object_id);
    uqa_sql::schema::domains::constraints::materialize(&mut definition, &mut allocator).map_err(
        |error| uqa_sql::catalog::errors::storage_error("domain constraint identity", &error),
    )?;
    let object_id = (context.allocate_identity)()?;
    let oid = domain_object_oid(&object_id);
    context.creation.retain_owner(&owner)?;
    let before = context.publication.domain_registry().clone();
    let mut registry = before.clone();
    registry.insert(
        identity.qualified_name(),
        StoredDomain {
            object_id,
            oid,
            identity,
            owner: owner.identity(),
            definition,
        },
    );
    domain::publish(context.publication, &before, registry)?;
    context.changes.catalog_registry_changed();
    Ok(())
}

pub mod dependencies;
pub mod removal;
