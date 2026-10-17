//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range definitions publish their four type claims in the caller's definition transaction. Generated routines remain in the routine catalog.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Deref,
    sync::Arc,
};
use uqa_sql::catalog::{
    range_type::{validate_range_registry, StoredRange},
    roles::RoleDefinition,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

use super::type_records::{TypeRecord, TypeRecordFormat};

mod descriptors;
pub mod input;
pub mod support;
mod validation;
pub(crate) use descriptors::RangeDescriptors;
pub use validation::validate_catalog;

pub type RangeRegistry = BTreeMap<String, StoredRange>;
pub type RangeRegistryRead<'a> = Box<dyn Deref<Target = RangeRegistry> + 'a>;

pub trait RangeRegistryPublication {
    fn range_registry(&self) -> RangeRegistryRead<'_>;
    fn range_catalog(&self) -> Option<&dyn CatalogFacade>;
    fn range_catalog_snapshot(&self) -> super::CatalogReadView;
    fn range_role_definitions(&self) -> BTreeMap<String, RoleDefinition>;
    fn publish_range_definitions(&self, registry: RangeRegistry);
}

const FORMAT: TypeRecordFormat = TypeRecordFormat {
    label: "range",
    definition_context: "range definition",
    oid_context: "range OID",
    format_key: "sql_range_types_json",
    format_field: "range_catalog_format",
    version: 1,
    prefix: "uqa.sql.range.v1:",
    oid_prefix: "uqa.sql.range_oid.v1:",
};

impl TypeRecord for StoredRange {
    fn claimed_oids(&self) -> BTreeSet<u32> {
        self.type_oids().into_iter().collect()
    }
}

pub fn restore(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<RangeRegistry> {
    let registry = FORMAT.read(catalog)?;
    validate_range_registry(&registry, roles).map_err(StorageBackendError::Other)?;
    Ok(registry)
}

pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    current: &Arc<RangeRegistry>,
    committed: Arc<RangeRegistry>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<Arc<RangeRegistry>> {
    let registry = FORMAT.merge_private(catalog, current, committed)?;
    validate_range_registry(&registry, roles).map_err(StorageBackendError::Other)?;
    Ok(registry)
}

/// Persist independent names and OID claims before replacing the live generation. The surrounding definition transaction owns rollback on any storage failure.
pub fn publish(
    publication: &dyn RangeRegistryPublication,
    before: &RangeRegistry,
    registry: RangeRegistry,
) -> Result<(), uqa_sql::SQLError> {
    let mut snapshot = publication.range_catalog_snapshot().snapshot().clone();
    let registry = {
        let current = publication.range_registry();
        let registry = FORMAT
            .merge_changes(before, &registry, &current)
            .map_err(|error| {
                uqa_sql::catalog::errors::storage_error("prepare range catalog", &error)
            })?;
        validate_range_registry(&registry, &publication.range_role_definitions())
            .map_err(uqa_sql::SQLError::Internal)?;
        snapshot.definitions.ranges = Arc::new(registry.clone());
        validate_catalog(&super::CatalogReadView::new(snapshot))?;
        if let Some(catalog) = publication.range_catalog() {
            FORMAT
                .persist(catalog, &current, &registry)
                .map_err(|error| {
                    uqa_sql::catalog::errors::storage_error("persist range catalog", &error)
                })?;
        }
        registry
    };
    publication.publish_range_definitions(registry);
    Ok(())
}

#[cfg(test)]
mod tests;
