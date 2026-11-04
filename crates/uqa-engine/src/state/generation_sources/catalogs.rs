//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Register the original immutable definitions with their owning durable producers.

use crate::state::{
    CapturedDatabaseAuthority, CatalogCell, DatabaseAuthority, DurableCatalogSnapshot,
    SnapshotTableSources,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::catalog::roles::{RoleDefinitions, RoleMemberships};
use uqa_sql::catalog::security::schema_registry::SchemaRegistry;
use uqa_sql::catalog::{
    composite_type::StoredComposite,
    definition_capture::routines::CapturedRoutines,
    domain::StoredDomain,
    enum_type::StoredEnum,
    events::{RuleCatalog, TriggerCatalog},
    foreign_server::ForeignServers,
    foreign_wrapper::ForeignWrappers,
    graph_oids::{CapturedGraphOidRegistry, GraphOidRegistry},
    range_type::StoredRange,
    shell_type::StoredShellType,
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
    domains: uqa_sql::catalog::type_definitions::TypeDefinitions<StoredDomain> => uqa_sql::catalog::type_definitions::CapturedTypeDefinitions<StoredDomain>, uqa_sql::catalog::type_definitions::TypeDefinitions::capture;
    enums: uqa_sql::catalog::type_definitions::TypeDefinitions<StoredEnum> => uqa_sql::catalog::type_definitions::CapturedTypeDefinitions<StoredEnum>, uqa_sql::catalog::type_definitions::TypeDefinitions::capture;
    composites: uqa_sql::catalog::type_definitions::TypeDefinitions<StoredComposite> => uqa_sql::catalog::type_definitions::CapturedTypeDefinitions<StoredComposite>, uqa_sql::catalog::type_definitions::TypeDefinitions::capture;
    ranges: uqa_sql::catalog::type_definitions::TypeDefinitions<StoredRange> => uqa_sql::catalog::type_definitions::CapturedTypeDefinitions<StoredRange>, uqa_sql::catalog::type_definitions::TypeDefinitions::capture;
    shells: uqa_sql::catalog::type_definitions::TypeDefinitions<StoredShellType> => uqa_sql::catalog::type_definitions::CapturedTypeDefinitions<StoredShellType>, uqa_sql::catalog::type_definitions::TypeDefinitions::capture;
    schemas: SchemaRegistry => uqa_sql::catalog::security::schema_registry::CapturedSchemaRegistry, SchemaRegistry::capture;
    views: uqa_sql::catalog::stored_view::ViewDefinitions => uqa_sql::catalog::stored_view::CapturedViewDefinitions, uqa_sql::catalog::stored_view::ViewDefinitions::capture;
    sql_user_functions: BTreeMap<String, Vec<Arc<uqa_sql::routines::SQLUserFunction>>> => CapturedRoutines, uqa_sql::catalog::definition_capture::routines::routines;
    roles: RoleDefinitions => uqa_sql::catalog::roles::CapturedRoleDefinitions, RoleDefinitions::capture;
    role_memberships: RoleMemberships => uqa_sql::catalog::roles::CapturedRoleMemberships, RoleMemberships::capture;
    database_security: DatabaseAuthority => CapturedDatabaseAuthority, DatabaseAuthority::capture;
    builtin_routine_security: uqa_sql::catalog::security::builtin_routines::BuiltinRoutineSecurities => uqa_sql::catalog::security::builtin_routines::CapturedBuiltinRoutineSecurities, uqa_sql::catalog::security::builtin_routines::BuiltinRoutineSecurities::capture;
    system_relation_security: uqa_sql::catalog::security::system_relations::SystemRelationSecurities => uqa_sql::catalog::security::system_relations::CapturedSystemRelationSecurities, uqa_sql::catalog::security::system_relations::SystemRelationSecurities::capture;
    foreign_wrappers: ForeignWrappers => uqa_sql::catalog::foreign_wrapper::CapturedForeignWrappers, ForeignWrappers::capture;
    foreign_servers: ForeignServers => uqa_sql::catalog::foreign_server::CapturedForeignServers, ForeignServers::capture;
    triggers: TriggerCatalog => uqa_sql::catalog::definition_capture::events::CapturedTriggers, uqa_sql::catalog::definition_capture::events::durable_triggers;
    rules: RuleCatalog => uqa_sql::catalog::definition_capture::rules::CapturedRules, uqa_sql::catalog::definition_capture::rules::durable_rules;
    graph_catalog_oids: GraphOidRegistry => CapturedGraphOidRegistry, GraphOidRegistry::capture;
}
