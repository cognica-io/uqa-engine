//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Register the original immutable definitions with their owning durable producers.

use crate::state::{
    BoundDatabaseSecurity, CatalogCell, DurableCatalogSnapshot, SnapshotTableSources,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::security::schema_registry::SchemaRegistry;
use uqa_sql::catalog::{
    composite_type::StoredComposite,
    definition_capture::{routines::CapturedRoutines, views::CapturedViews},
    domain::StoredDomain,
    enum_type::StoredEnum,
    events::{RuleCatalog, TriggerCatalog},
    foreign_server::ForeignServerDefinition,
    foreign_wrapper::ForeignWrappers,
    graph_oids::GraphCatalogOids,
    range_type::StoredRange,
    roles::{RoleDefinition, RoleMembership, RoleMembershipKey},
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
    domains: uqa_sql::catalog::type_definitions::TypeDefinitions<StoredDomain> => uqa_sql::catalog::type_definitions::CapturedTypeDefinitions<StoredDomain>, uqa_sql::catalog::type_definitions::TypeDefinitions::capture;
    enums: uqa_sql::catalog::type_definitions::TypeDefinitions<StoredEnum> => uqa_sql::catalog::type_definitions::CapturedTypeDefinitions<StoredEnum>, uqa_sql::catalog::type_definitions::TypeDefinitions::capture;
    composites: uqa_sql::catalog::type_definitions::TypeDefinitions<StoredComposite> => uqa_sql::catalog::type_definitions::CapturedTypeDefinitions<StoredComposite>, uqa_sql::catalog::type_definitions::TypeDefinitions::capture;
    ranges: uqa_sql::catalog::type_definitions::TypeDefinitions<StoredRange> => uqa_sql::catalog::type_definitions::CapturedTypeDefinitions<StoredRange>, uqa_sql::catalog::type_definitions::TypeDefinitions::capture;
    shells: uqa_sql::catalog::type_definitions::TypeDefinitions<StoredShellType> => uqa_sql::catalog::type_definitions::CapturedTypeDefinitions<StoredShellType>, uqa_sql::catalog::type_definitions::TypeDefinitions::capture;
    schemas: SchemaRegistry => uqa_sql::catalog::security::schema_registry::CapturedSchemaRegistry, SchemaRegistry::capture;
    views: BTreeMap<RelationIdentity, StoredView> => CapturedViews, uqa_sql::catalog::definition_capture::views::durable_views;
    sql_user_functions: BTreeMap<String, Vec<Arc<uqa_sql::routines::SQLUserFunction>>> => CapturedRoutines, uqa_sql::catalog::definition_capture::routines::routines;
    roles: BTreeMap<String, RoleDefinition> => Vec<(String, uqa_sql::catalog::roles::capture::CapturedRoleDefinition)>, uqa_sql::catalog::roles::capture::roles;
    role_memberships: BTreeMap<RoleMembershipKey, RoleMembership> => Vec<(RoleMembershipKey, RoleMembership)>, uqa_sql::catalog::roles::capture::memberships;
    database_security: BoundDatabaseSecurity => BoundDatabaseSecurity, BoundDatabaseSecurity::capture_security;
    builtin_routine_security: uqa_sql::catalog::security::builtin_routines::BuiltinRoutineSecurities => Vec<(u32, uqa_sql::catalog::security::builtin_routines::BuiltinRoutineSecurity)>, uqa_sql::catalog::security::capture::relations::builtin_routines;
    system_relation_security: uqa_sql::catalog::security::system_relations::SystemRelationSecurities => Vec<(RelationIdentity, uqa_sql::catalog::security::capture::relations::CapturedSystemRelationSecurity)>, uqa_sql::catalog::security::capture::relations::system_relations;
    foreign_wrappers: ForeignWrappers => Vec<(String, uqa_sql::catalog::foreign_wrapper::ForeignWrapperDefinition)>, uqa_sql::catalog::definition_capture::foreign::wrappers;
    foreign_servers: BTreeMap<String, ForeignServerDefinition> => Vec<(String, uqa_sql::catalog::definition_capture::foreign::CapturedForeignServer)>, uqa_sql::catalog::definition_capture::foreign::servers;
    triggers: TriggerCatalog => uqa_sql::catalog::definition_capture::events::CapturedTriggers, uqa_sql::catalog::definition_capture::events::durable_triggers;
    rules: RuleCatalog => uqa_sql::catalog::definition_capture::rules::CapturedRules, uqa_sql::catalog::definition_capture::rules::durable_rules;
    graph_catalog_oids: BTreeMap<String, GraphCatalogOids> => Vec<(String, uqa_sql::catalog::definition_capture::identities::CapturedGraphCatalogOids)>, uqa_sql::catalog::definition_capture::identities::graph_oids;
}
