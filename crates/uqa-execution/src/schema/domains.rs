//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain declaration scheduling and publication through current namespace and binding inputs.

use super::namespaces::SchemaStatementWriter;
use uqa_sql::{
    ast::CreateDomain,
    catalog::{
        domain::{domain_object_oid, StoredDomain},
        roles::RoleReferenceNames,
    },
    schema::domains::{bind_domain_creation_target, DomainCreationCatalog},
    SQLError,
};

pub trait DomainDeclarationBinding {
    fn bind_domain_declaration(&self, definition: &mut CreateDomain) -> Result<(), SQLError>;
}
pub trait DomainPublication {
    fn publish_domain(&self, domain: StoredDomain) -> Result<(), SQLError>;
}
pub struct DomainCreationContext<'a> {
    pub writer: &'a dyn SchemaStatementWriter,
    pub catalog: &'a dyn DomainCreationCatalog,
    pub bindings: &'a dyn DomainDeclarationBinding,
    pub allocate_identity: fn() -> Result<[u8; 16], SQLError>,
    pub session: &'a dyn RoleReferenceNames,
    pub publication: &'a dyn DomainPublication,
}

pub fn create_domain(
    context: &DomainCreationContext<'_>,
    mut definition: CreateDomain,
) -> Result<(), SQLError> {
    context.writer.prepare_writer()?;
    let identity = bind_domain_creation_target(context.catalog, &mut definition)?;
    context.bindings.bind_domain_declaration(&mut definition)?;
    let object_id = (context.allocate_identity)()?;
    let oid = domain_object_oid(&object_id);
    context.publication.publish_domain(StoredDomain {
        object_id,
        oid,
        identity,
        owner: context.session.current_user_name(),
        definition,
    })
}

pub mod dependencies;
pub mod removal;
