//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Register the original immutable definitions with their owning durable producers.

use crate::state::{CatalogCell, DurableCatalogSnapshot, SnapshotTableSources};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::{
    composite_type::StoredComposite,
    definition_capture::{routines::CapturedRoutines, views::CapturedViews},
    domain::StoredDomain,
    enum_type::StoredEnum,
    range_type::StoredRange,
    security::BoundSchemaSecurity,
    shell_type::StoredShellType,
    stored_view::StoredView,
};
use uqa_storage::{memory_capture::MemoryCaptureRegistry, StorageBackendResult};

macro_rules! catalog_sources {
    ($($field:ident: $source:ty => $image:ty, $producer:path;)+) => {
        pub(super) struct CatalogSources {
            $($field: CatalogCell<$source, $image>,)+
        }

        impl CatalogSources {
            pub(super) fn new(registry: &MemoryCaptureRegistry, durable: &DurableCatalogSnapshot) -> StorageBackendResult<Self> {
                Ok(Self {
                    $($field: CatalogCell::capture_source(durable.$field.clone(), Some(registry), $producer)?,)+
                })
            }

            pub(super) fn select(&self, tables: &mut SnapshotTableSources) -> StorageBackendResult<()> {
                $(tables.select(&self.$field.key()?)?;)+
                Ok(())
            }

            #[cfg(test)]
            pub(super) fn keys(&self, control: &uqa_storage::read_control::StorageReadControl) -> StorageBackendResult<uqa_core::memory::Budgeted<CatalogKeys>> {
                let keys = CatalogKeys { $($field: self.$field.key()?,)+ };
                let mut bytes = size_of::<CatalogKeys>();
                $(bytes = bytes.checked_add(keys.$field.retained_source_bytes()).ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;)+
                Ok(uqa_core::memory::Budgeted::new(keys, control.memory().reserve(bytes)?))
            }
        }

        #[cfg(test)]
        pub(crate) struct CatalogKeys {
            $(pub(crate) $field: uqa_storage::memory_capture::MemoryCaptureKey<uqa_storage::memory_capture::CapturedMemoryValue<$image>>,)+
        }
    }
}

catalog_sources! {
    domains: BTreeMap<String, StoredDomain> => Vec<(String, StoredDomain)>, uqa_sql::catalog::definition_capture::types::domains;
    enums: BTreeMap<String, StoredEnum> => Vec<(String, StoredEnum)>, uqa_sql::catalog::definition_capture::types::enums;
    composites: BTreeMap<String, StoredComposite> => Vec<(String, StoredComposite)>, uqa_sql::catalog::definition_capture::types::composites;
    ranges: BTreeMap<String, StoredRange> => Vec<(String, StoredRange)>, uqa_sql::catalog::definition_capture::types::ranges;
    shells: BTreeMap<String, StoredShellType> => Vec<(String, StoredShellType)>, uqa_sql::catalog::definition_capture::types::shells;
    schemas: BTreeMap<String, BoundSchemaSecurity> => Vec<(String, BoundSchemaSecurity)>, uqa_sql::catalog::security::capture::schemas;
    views: BTreeMap<RelationIdentity, StoredView> => CapturedViews, uqa_sql::catalog::definition_capture::views::durable_views;
    sql_user_functions: BTreeMap<String, Vec<Arc<uqa_sql::routines::SQLUserFunction>>> => CapturedRoutines, uqa_sql::catalog::definition_capture::routines::routines;
}
