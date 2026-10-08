//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The lifecycle of user-defined types after creation: rename, schema moves, ownership and `USAGE` privileges of enums and domains, whose generated array types follow them.

use super::namespaces::{
    relations::RelationCreationContext, NamespaceCatalogChanges, SchemaStatementWriter,
};
use crate::catalog::{identity::CatalogIdentityReservationContext, notices::CatalogNotices};
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::security::ownership::RelationOwnerSchemas,
    schema::domains::removal::TypeObjectBinding, SQLError,
};

pub mod arrays;
mod lifecycle;
mod privileges;
pub mod relation_arrays;

pub(crate) use lifecycle::lock_named_type;
pub use lifecycle::{alter_type_object, relocate_type_object};
pub use privileges::grant_type;

/// Stored definitions outside the type registries that embed a user-defined type's catalog name: table and foreign table columns and view output types. A rename or schema move rewrites them in the caller's transaction.
pub trait TypeReferenceDependents {
    fn rewrite_type_references(
        &self,
        oid: u32,
        identity: &RelationIdentity,
    ) -> Result<(), SQLError>;
}

pub struct TypeLifecycleContext<'a> {
    pub composite_attributes: super::composites::alteration::CompositeAlterationContext<'a>,
    pub creation: RelationCreationContext<'a>,
    pub identities: CatalogIdentityReservationContext<'a>,
    pub writer: &'a dyn SchemaStatementWriter,
    pub binding: TypeObjectBinding<'a>,
    pub registries: arrays::UserTypeRegistries<'a>,
    pub owner_schemas: &'a dyn RelationOwnerSchemas,
    pub changes: &'a dyn NamespaceCatalogChanges,
    pub notices: &'a dyn CatalogNotices,
    pub dependents: &'a dyn TypeReferenceDependents,
}
