//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Standalone composite type creation: namespace and type-name reservation, attribute checks, the composite relation's name and OIDs, and registry publication.

use super::namespaces::{
    relations::RelationCreationContext, NamespaceCatalogChanges, SchemaStatementWriter,
};
use crate::catalog::{
    composite_type::{self, CompositeRegistryPublication},
    domain::DomainRegistryPublication,
    enum_type::EnumRegistryPublication,
    identity::{allocate_catalog_object_id, CatalogIdentityReservationContext},
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::CreateCompositeType,
    catalog::{composite_type::StoredComposite, relation_oids::RelationOidKind},
    type_resolution::FunctionTypeResolver,
    SQLError,
};

mod addition;
pub mod alteration;
pub mod attributes;
pub mod catalog_values;
pub mod values;

pub struct CompositeTypeContext<'a> {
    pub creation: RelationCreationContext<'a>,
    pub identities: CatalogIdentityReservationContext<'a>,
    pub writer: &'a dyn SchemaStatementWriter,
    /// Resolves attribute types through the catalog and checks `USAGE` on them.
    pub types: &'a dyn FunctionTypeResolver,
    pub allocate_identity: fn() -> Result<[u8; 16], SQLError>,
    pub publication: &'a dyn CompositeRegistryPublication,
    /// Enum and domain arrays share the type namespace with composite arrays.
    pub enums: &'a dyn EnumRegistryPublication,
    pub domains: &'a dyn DomainRegistryPublication,
    pub changes: &'a dyn NamespaceCatalogChanges,
}

/// `CREATE TYPE ... AS (...)`, as `DefineCompositeType` and `DefineRelation` check it: the creation namespace, then the type name, where a generated array holding the name moves aside and any other type is a 42710 conflict, then the attributes, then the composite relation's name, and then `heap_create_with_catalog` allocates the relation, array type and row type OIDs.
pub fn create_composite_type(
    context: &CompositeTypeContext<'_>,
    definition: CreateCompositeType,
) -> Result<(), SQLError> {
    let owner = context.creation.bind_owner()?;
    context.writer.prepare_writer()?;
    let name = context.creation.persistent_name(&definition.name)?;
    let requested = RelationIdentity::from_legacy_name(&name).map_err(SQLError::Internal)?;
    super::types::arrays::displace_array_type(
        &context.creation,
        super::types::arrays::UserTypeRegistries {
            enums: context.enums,
            domains: context.domains,
            composites: context.publication,
        },
        &requested,
    )?;
    let identity = context.creation.reserve_type_name(&name)?;
    let attributes = uqa_sql::schema::composites::prepare_composite_attributes(
        context.types,
        &definition.attributes,
    )?;
    if context.creation.relation_name_in_use(&identity) {
        return Err(SQLError::Routine {
            sqlstate: "42P07".into(),
            message: format!("relation \"{}\" already exists", identity.name),
        });
    }
    context.creation.reserve_name(&name)?;
    let array_name = super::types::arrays::reserve_array_name(
        &context.creation,
        &identity.schema,
        &identity.name,
    )?;
    let object_id = (context.allocate_identity)()?;
    let oids = context
        .identities
        .allocator(allocate_catalog_object_id)
        .allocate_relation_oids(RelationOidKind::CompositeType, &identity)?;
    let (Some(array_oid), Some(oid)) = (oids.array_type, oids.row_type) else {
        return Err(SQLError::Internal(
            "composite type relation OIDs omitted its types".into(),
        ));
    };
    context.creation.retain_owner(&owner)?;
    let before = context.publication.composite_registry().clone();
    let mut registry = before.clone();
    registry.insert(
        identity.qualified_name(),
        StoredComposite {
            object_id,
            oid,
            relation_oid: oids.relation,
            array_oid,
            array_name,
            identity,
            owner: owner.identity(),
            attributes,
            usage_acl: None,
        },
    );
    composite_type::publish(context.publication, &before, registry)?;
    context.changes.catalog_registry_changed();
    Ok(())
}
