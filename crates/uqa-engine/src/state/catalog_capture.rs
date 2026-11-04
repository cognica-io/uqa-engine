//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memory catalog generations register owning producers before Engine publication.

use super::{BoundDatabaseSecurity, CatalogCell, DurableCatalogState};
use uqa_sql::catalog::roles::{RoleDefinitions, RoleMemberships};
use uqa_storage::{memory_capture::MemoryCaptureRegistry, StorageBackendResult};

impl DurableCatalogState {
    pub(super) fn register_catalog_sources(
        &mut self,
        registry: &MemoryCaptureRegistry,
    ) -> StorageBackendResult<()> {
        self.register_type_sources(registry)?;
        self.register_metadata_sources(registry)?;
        self.register_foreign_sources(registry)?;
        self.views = CatalogCell::capture_source(
            self.views.snapshot(),
            Some(registry),
            uqa_sql::catalog::definition_capture::views::durable_views,
        )?;
        self.triggers = CatalogCell::capture_source(
            self.triggers.snapshot(),
            Some(registry),
            uqa_sql::catalog::definition_capture::events::triggers,
        )?;
        self.sql_user_functions = CatalogCell::capture_source(
            self.sql_user_functions.snapshot(),
            Some(registry),
            uqa_sql::catalog::definition_capture::routines::routines,
        )?;
        self.rules = CatalogCell::capture_source(
            self.rules.snapshot(),
            Some(registry),
            uqa_sql::catalog::definition_capture::rules::rules,
        )?;
        self.register_authority_sources(registry)
    }

    fn register_type_sources(
        &mut self,
        registry: &MemoryCaptureRegistry,
    ) -> StorageBackendResult<()> {
        self.domains = CatalogCell::capture_source(
            self.domains.snapshot(),
            Some(registry),
            uqa_sql::catalog::type_definitions::TypeDefinitions::capture,
        )?;
        self.enums = CatalogCell::capture_source(
            self.enums.snapshot(),
            Some(registry),
            uqa_sql::catalog::type_definitions::TypeDefinitions::capture,
        )?;
        self.composites = CatalogCell::capture_source(
            self.composites.snapshot(),
            Some(registry),
            uqa_sql::catalog::type_definitions::TypeDefinitions::capture,
        )?;
        self.ranges = CatalogCell::capture_source(
            self.ranges.snapshot(),
            Some(registry),
            uqa_sql::catalog::type_definitions::TypeDefinitions::capture,
        )?;
        self.shells = CatalogCell::capture_source(
            self.shells.snapshot(),
            Some(registry),
            uqa_sql::catalog::type_definitions::TypeDefinitions::capture,
        )?;
        Ok(())
    }

    fn register_foreign_sources(
        &mut self,
        registry: &MemoryCaptureRegistry,
    ) -> StorageBackendResult<()> {
        self.foreign_wrappers = CatalogCell::capture_source(
            self.foreign_wrappers.snapshot(),
            Some(registry),
            uqa_sql::catalog::foreign_wrapper::ForeignWrappers::capture,
        )?;
        self.foreign_servers = CatalogCell::capture_source(
            self.foreign_servers.snapshot(),
            Some(registry),
            uqa_sql::catalog::foreign_server::ForeignServers::capture,
        )?;
        self.foreign_tables = CatalogCell::capture_source(
            self.foreign_tables.snapshot(),
            Some(registry),
            uqa_execution::catalog::foreign::capture::tables,
        )?;
        Ok(())
    }

    fn register_authority_sources(
        &mut self,
        registry: &MemoryCaptureRegistry,
    ) -> StorageBackendResult<()> {
        self.database_security = CatalogCell::capture_source(
            self.database_security.snapshot(),
            Some(registry),
            BoundDatabaseSecurity::capture_security,
        )?;
        self.schemas = CatalogCell::capture_source(
            self.schemas.snapshot(),
            Some(registry),
            uqa_sql::catalog::security::schema_registry::SchemaRegistry::capture,
        )?;
        self.sequence_security = CatalogCell::capture_source(
            self.sequence_security.snapshot(),
            Some(registry),
            uqa_sql::catalog::security::capture::sequences,
        )?;
        self.roles = CatalogCell::capture_source(
            self.roles.snapshot(),
            Some(registry),
            RoleDefinitions::capture,
        )?;
        self.role_memberships = CatalogCell::capture_source(
            self.role_memberships.snapshot(),
            Some(registry),
            RoleMemberships::capture,
        )?;
        self.foreign_table_security = CatalogCell::capture_source(
            self.foreign_table_security.snapshot(),
            Some(registry),
            uqa_sql::catalog::security::capture::relations::tables,
        )?;
        self.builtin_routine_security = CatalogCell::capture_source(
            self.builtin_routine_security.snapshot(),
            Some(registry),
            uqa_sql::catalog::security::builtin_routines::BuiltinRoutineSecurities::capture,
        )?;
        self.system_relation_security = CatalogCell::capture_source(
            self.system_relation_security.snapshot(),
            Some(registry),
            uqa_sql::catalog::security::system_relations::SystemRelationSecurities::capture,
        )?;
        Ok(())
    }

    fn register_metadata_sources(
        &mut self,
        registry: &MemoryCaptureRegistry,
    ) -> StorageBackendResult<()> {
        self.comments = CatalogCell::capture_source(
            self.comments.snapshot(),
            Some(registry),
            uqa_execution::catalog::comments::capture::comments,
        )?;
        self.sequences = CatalogCell::capture_source(
            self.sequences.snapshot(),
            Some(registry),
            uqa_execution::catalog::sequence::capture::sequences,
        )?;
        self.sequence_object_ids = CatalogCell::capture_source(
            self.sequence_object_ids.snapshot(),
            Some(registry),
            uqa_sql::catalog::definition_capture::identities::sequence_objects,
        )?;
        self.sequence_catalog_oids = CatalogCell::capture_source(
            self.sequence_catalog_oids.snapshot(),
            Some(registry),
            uqa_sql::catalog::definition_capture::identities::sequence_oids,
        )?;
        self.sequence_persistence = CatalogCell::capture_source(
            self.sequence_persistence.snapshot(),
            Some(registry),
            uqa_sql::catalog::definition_capture::identities::sequence_persistence,
        )?;
        self.graph_catalog_oids = CatalogCell::capture_source(
            self.graph_catalog_oids.snapshot(),
            Some(registry),
            uqa_sql::catalog::graph_oids::GraphOidRegistry::capture,
        )?;
        self.catalog_indexes = CatalogCell::capture_resource(
            self.catalog_indexes.snapshot(),
            registry,
            uqa_storage::catalog::capture::indexes,
        )?;
        self.scoring_params = CatalogCell::capture_resource(
            self.scoring_params.snapshot(),
            registry,
            uqa_storage::catalog::scoring_params::ScoringParameters::capture,
        )?;
        Ok(())
    }
}
