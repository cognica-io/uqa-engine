//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore type definitions early and finalize constraint addresses against the complete catalog.

use super::{records, DomainRegistry};
use crate::catalog::{projection, CatalogReadView, RelationNameResolution};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::ConstraintCatalogIdentity,
    catalog::{
        domain::{validate_domain_definitions, validate_domain_registry},
        roles::RoleDefinition,
    },
    schema::{
        constraint_metadata::{
            CatalogObjectAllocator, CatalogOidClass, ConstraintMetadataError,
            ConstraintMetadataResult,
        },
        domains::constraints,
    },
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

#[derive(Debug)]
pub struct DomainRestoreState {
    current: bool,
}

pub struct RestoredDomains {
    pub registry: DomainRegistry,
    pub state: DomainRestoreState,
}

/// This read never writes conversion metadata; later relation restoration still needs these domain definitions for type binding.
pub fn restore(
    storage: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
    allow_migration: bool,
) -> StorageBackendResult<RestoredDomains> {
    let (registry, current) = records::read(storage, roles, allow_migration)?;
    if current {
        validate_domain_registry(&registry, roles)
    } else {
        validate_domain_definitions(&registry, roles)
    }
    .map_err(invalid)?;
    Ok(RestoredDomains {
        registry,
        state: DomainRestoreState { current },
    })
}

/// Initial open owns the enclosing transaction. Validate every supplied address before allocating or persisting any missing legacy metadata.
pub fn finish_restore(
    storage: &dyn CatalogFacade,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    state: DomainRestoreState,
) -> StorageBackendResult<Option<DomainRegistry>> {
    let mut bound_resolution = resolution.clone();
    bound_resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
    let resolution = &bound_resolution;
    let domains = &catalog.snapshot().definitions.domains;
    let roles = &catalog.snapshot().definitions.roles;
    validate_domain_definitions(domains, roles).map_err(invalid)?;
    for domain in domains.values() {
        for identity in constraints::identities(&domain.definition) {
            projection::validate_catalog_identity_claim(
                catalog,
                resolution,
                &domain.identity,
                CatalogOidClass::Constraint,
                identity,
            )
            .map_err(invalid)?;
        }
    }
    if state.current {
        validate_domain_registry(domains, roles).map_err(invalid)?;
        return Ok(None);
    }
    let mut registry = (**domains).clone();
    let mut schemas = BTreeMap::new();
    let mut allocator = RestorationAllocator {
        catalog,
        resolution,
        assigned: BTreeSet::new(),
    };
    for domain in registry.values_mut() {
        let names = schemas
            .entry(domain.identity.schema.clone())
            .or_insert_with(|| {
                crate::schema::constraints::names::schema_names(catalog, &domain.identity.schema)
            });
        constraints::assign_names(&mut domain.definition, names).map_err(invalid)?;
        names.extend(
            domain
                .definition
                .checks
                .iter()
                .filter_map(|check| check.name.clone()),
        );
        names.extend(
            domain
                .definition
                .not_null
                .iter()
                .filter_map(|constraint| constraint.name.clone()),
        );
        constraints::materialize(&mut domain.definition, &mut allocator).map_err(invalid)?;
    }
    validate_domain_registry(&registry, roles).map_err(invalid)?;
    records::migrate(storage, &registry)?;
    Ok(Some(registry))
}

struct RestorationAllocator<'a> {
    catalog: &'a CatalogReadView,
    resolution: &'a RelationNameResolution,
    assigned: BTreeSet<i64>,
}

impl CatalogObjectAllocator for RestorationAllocator<'_> {
    fn include_catalog_identity(
        &mut self,
        relation: &RelationIdentity,
        class: CatalogOidClass,
        identity: ConstraintCatalogIdentity,
    ) -> ConstraintMetadataResult<()> {
        projection::validate_catalog_identity_claim(
            self.catalog,
            self.resolution,
            relation,
            class,
            identity,
        )
        .map_err(|error| ConstraintMetadataError::Execution(Box::new(error)))?;
        self.assigned.insert(identity.oid);
        Ok(())
    }
    fn allocate_object_id(&mut self, kind: &str) -> ConstraintMetadataResult<[u8; 16]> {
        crate::catalog::identity::allocate_catalog_object_id(kind)
    }
    fn allocate_catalog_oid(
        &mut self,
        class: CatalogOidClass,
        object_id: &[u8; 16],
    ) -> ConstraintMetadataResult<i64> {
        let mut oid = uqa_sql::catalog::oids::stable_object_oid(class.label(), object_id);
        loop {
            if oid >= 16_384
                && !self.assigned.contains(&oid)
                && !projection::catalog_oid_in_use(self.catalog, self.resolution, class, oid)
                    .map_err(|error| ConstraintMetadataError::Execution(Box::new(error)))?
            {
                self.assigned.insert(oid);
                return Ok(oid);
            }
            oid = crate::catalog::identity::allocate_catalog_oid(class.label())
                .map_err(|error| ConstraintMetadataError::Execution(Box::new(error)))?;
        }
    }
}

fn invalid(error: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::Other(format!("domain constraint catalog: {error}"))
}
