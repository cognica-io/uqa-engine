//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind user-defined type lifecycle DDL to namespace, identity, role, registry and dependent-definition state.

use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_execution::schema::types::{
    arrays::UserTypeRegistries, TypeLifecycleContext, TypeReferenceDependents,
};
use uqa_sql::schema::domains::removal::TypeObjectBinding;
use uqa_sql::SQLError;

impl Engine {
    pub(crate) fn relation_array_context(
        &self,
    ) -> uqa_execution::schema::types::relation_arrays::RelationArrayContext<'_> {
        uqa_execution::schema::types::relation_arrays::RelationArrayContext {
            catalog: self,
            tables: self,
            views: self,
            foreign: self,
            foreign_publication: self,
            storage: self.storage.catalog.as_deref(),
            changes: self,
            relations: self,
            locks: self,
        }
    }
    pub(crate) fn type_lifecycle_context(&self) -> TypeLifecycleContext<'_> {
        TypeLifecycleContext {
            composite_attributes:
                uqa_execution::schema::composites::addition::CompositeAdditionContext {
                    binding: self.table_alter_binding_context(),
                    attributes: self.composite_attribute_context(),
                    types: self,
                    sequences: self.implicit_sequence_context(),
                },
            creation: self.relation_creation_context(),
            identities: self.catalog_identity_reservation_context(),
            writer: self,
            binding: TypeObjectBinding {
                catalog: self,
                authority: self,
                session: self,
            },
            registries: UserTypeRegistries {
                enums: self,
                domains: self,
                composites: self,
            },
            owner_schemas: self,
            changes: self,
            notices: self,
            dependents: self,
        }
    }
}

impl TypeReferenceDependents for Engine {
    fn rewrite_type_references(
        &self,
        oid: u32,
        identity: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.rewrite_schema_type_references(oid, identity)
            .map_err(|error| SQLError::Internal(format!("rewrite type references: {error}")))?;
        uqa_execution::schema::view_dependencies::rewrite_view_type_references(
            &self.view_dependency_context(),
            oid,
            identity,
        )
        .map_err(|error| SQLError::Internal(format!("rewrite view type references: {error}")))
    }
}
