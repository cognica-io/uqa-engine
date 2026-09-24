//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply declaration materialization with transaction-owned catalog address reservations.

use crate::catalog::services::{CatalogSession, CatalogSnapshotSource};
use crate::row_locks::shared_objects::SharedObjectLockSession;
use std::collections::BTreeSet;
use uqa_sql::schema::constraint_metadata::{
    CatalogObjectAllocator, CatalogOidClass, ConstraintMetadataError, ConstraintMetadataResult,
};

#[derive(Clone, Copy)]
pub struct CatalogIdentityReservationContext<'a> {
    pub catalog: &'a dyn CatalogSnapshotSource,
    pub session: &'a dyn CatalogSession,
    pub locks: &'a dyn SharedObjectLockSession,
}

impl<'a> CatalogIdentityReservationContext<'a> {
    pub fn allocator(
        self,
        allocate: fn(&str) -> ConstraintMetadataResult<[u8; 16]>,
    ) -> ReservedCatalogIdentityAllocator<'a> {
        ReservedCatalogIdentityAllocator {
            context: self,
            allocate,
            assigned: BTreeSet::new(),
        }
    }
}

pub struct ReservedCatalogIdentityAllocator<'a> {
    context: CatalogIdentityReservationContext<'a>,
    allocate: fn(&str) -> ConstraintMetadataResult<[u8; 16]>,
    assigned: BTreeSet<(CatalogOidClass, i64)>,
}

impl CatalogObjectAllocator for ReservedCatalogIdentityAllocator<'_> {
    fn include_catalog_identity(
        &mut self,
        relation: &uqa_core::RelationIdentity,
        class: CatalogOidClass,
        identity: uqa_sql::ast::ConstraintCatalogIdentity,
    ) -> ConstraintMetadataResult<()> {
        if !identity.is_valid() {
            return Err(ConstraintMetadataError::Invalid(
                "invalid supplied catalog identity".into(),
            ));
        }
        let mut resolution = self.context.session.relation_name_resolution();
        resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
        let exists = || {
            crate::catalog::projection::validate_catalog_identity_claim(
                &self.context.catalog.current_catalog_snapshot(),
                &resolution,
                relation,
                class,
                identity,
            )
        };
        if !exists().map_err(|error| ConstraintMetadataError::Execution(Box::new(error)))? {
            // Supplied addresses need the same exclusion and post-wait validation as generated ones; a conflict rejects the supplied identity instead of silently replacing it.
            super::reserve_catalog_oid(
                self.context.locks,
                class.class_id(),
                class.label(),
                |_| exists().map(|_| false),
                || Ok(identity.oid),
            )
            .map_err(|error| ConstraintMetadataError::Execution(Box::new(error)))?;
        }
        self.assigned.insert((class, identity.oid));
        Ok(())
    }

    fn allocate_object_id(&mut self, kind: &str) -> ConstraintMetadataResult<[u8; 16]> {
        (self.allocate)(kind)
    }

    fn allocate_catalog_oid(
        &mut self,
        class: CatalogOidClass,
        object_id: &[u8; 16],
    ) -> ConstraintMetadataResult<i64> {
        let mut proposed = Some(uqa_sql::catalog::oids::stable_object_oid(
            class.label(),
            object_id,
        ))
        .filter(|oid| *oid >= 16_384);
        let mut resolution = self.context.session.relation_name_resolution();
        resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
        let oid = super::reserve_catalog_oid(
            self.context.locks,
            class.class_id(),
            class.label(),
            |oid| {
                if self.assigned.contains(&(class, oid)) {
                    return Ok(true);
                }
                crate::catalog::projection::catalog_oid_in_use(
                    &self.context.catalog.current_catalog_snapshot(),
                    &resolution,
                    class,
                    oid,
                )
            },
            || match proposed.take() {
                Some(oid) => Ok(oid),
                None => super::allocate_catalog_oid(class.label()),
            },
        )
        .map_err(|error| ConstraintMetadataError::Execution(Box::new(error)))?;
        self.assigned.insert((class, oid));
        Ok(oid)
    }
}

#[cfg(test)]
mod tests;
