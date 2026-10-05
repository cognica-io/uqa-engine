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

impl ReservedCatalogIdentityAllocator<'_> {
    /// Allocate a new relation's OIDs in `heap_create_with_catalog`'s order: the relation, then, once `heap_create` accepts the relation's namespace, the array type `AssignTypeArrayOid` reserves, then the row type, and for a view the `_RETURN` rule `DefineViewRules` inserts next. Sequences have no row type. A refused relation has used its own OID.
    pub fn allocate_relation_oids(
        &mut self,
        kind: uqa_sql::catalog::relation_oids::RelationOidKind,
        relation: &uqa_core::RelationIdentity,
    ) -> Result<uqa_sql::catalog::relation_oids::RelationCatalogOids, uqa_sql::SQLError> {
        use uqa_sql::catalog::relation_oids::{RelationCatalogOids, RelationOidKind};
        let temporary_schema = self
            .context
            .session
            .relation_name_resolution()
            .temporary_schema;
        let mut allocate = |class| {
            self.allocate_catalog_oid(class, &[0; 16])
                .map_err(|error| uqa_sql::catalog::errors::storage_error("relation OID", &error))
                .and_then(|oid| {
                    u32::try_from(oid).map_err(|_| {
                        uqa_sql::SQLError::Internal(format!("invalid {} OID {oid}", class.label()))
                    })
                })
        };
        let relation_oid = allocate(CatalogOidClass::Relation)?;
        uqa_sql::catalog::resolution::creation::ensure_relation_namespace_writable(
            relation,
            &temporary_schema,
        )?;
        let (array_type, row_type) = if kind == RelationOidKind::Sequence {
            (None, None)
        } else {
            let array_type = allocate(CatalogOidClass::Type)?;
            (Some(array_type), Some(allocate(CatalogOidClass::Type)?))
        };
        let rule = if kind == RelationOidKind::View {
            Some(allocate(CatalogOidClass::Rewrite)?)
        } else {
            None
        };
        Ok(RelationCatalogOids {
            relation: relation_oid,
            row_type,
            array_type,
            rule,
        })
    }

    /// The next OID of the counter that the class does not hold and `accept` admits: `pg_enum` label allocation draws OIDs until one has the parity the label's sort position calls for.
    pub fn allocate_catalog_oid_matching(
        &mut self,
        class: CatalogOidClass,
        accept: impl Fn(i64) -> bool,
    ) -> Result<i64, uqa_sql::SQLError> {
        let mut resolution = self.context.session.relation_name_resolution();
        resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
        let locks = self.context.locks;
        let oid = super::reserve_catalog_oid(
            locks,
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
            || loop {
                let oid = i64::from(locks.next_catalog_oid()?);
                if accept(oid) {
                    return Ok(oid);
                }
            },
        )?;
        self.assigned.insert((class, oid));
        Ok(oid)
    }

    /// The next OID of the counter that no namespace holds, for a schema `NamespaceCreate` inserts; `namespace_in_use` tells whether a namespace already holds an OID.
    pub fn allocate_namespace_oid(
        &mut self,
        namespace_in_use: impl FnMut(i64) -> Result<bool, uqa_sql::SQLError>,
    ) -> Result<u32, uqa_sql::SQLError> {
        let oid = super::reserve_new_catalog_oid(
            self.context.locks,
            NAMESPACE_CLASS_ID,
            "schema",
            namespace_in_use,
        )?;
        u32::try_from(oid)
            .map_err(|_| uqa_sql::SQLError::Internal(format!("invalid schema OID {oid}")))
    }
}

/// `pg_namespace`'s catalog class.
const NAMESPACE_CLASS_ID: u32 = 2615;

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

    /// The next OID of the database's counter that the class does not hold; the object's identity does not select it.
    fn allocate_catalog_oid(
        &mut self,
        class: CatalogOidClass,
        _object_id: &[u8; 16],
    ) -> ConstraintMetadataResult<i64> {
        let mut resolution = self.context.session.relation_name_resolution();
        resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
        let oid = super::reserve_new_catalog_oid(
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
        )
        .map_err(|error| ConstraintMetadataError::Execution(Box::new(error)))?;
        self.assigned.insert((class, oid));
        Ok(oid)
    }
}

mod graphs;
pub use graphs::LabelShape;
mod temporary_namespaces;

#[cfg(test)]
mod tests;
