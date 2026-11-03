//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memory catalog generations register owning producers before Engine publication.

use super::{BoundDatabaseSecurity, CatalogCell, DurableCatalogState};
use uqa_storage::{memory_capture::MemoryCaptureRegistry, StorageBackendResult};

impl DurableCatalogState {
    pub(super) fn register_catalog_sources(
        &mut self,
        registry: &MemoryCaptureRegistry,
    ) -> StorageBackendResult<()> {
        self.register_type_sources(registry)?;
        self.register_metadata_sources(registry)?;
        self.register_authority_sources(registry)
    }

    fn register_type_sources(
        &mut self,
        registry: &MemoryCaptureRegistry,
    ) -> StorageBackendResult<()> {
        self.domains = CatalogCell::capture_source(
            self.domains.snapshot(),
            Some(registry),
            uqa_sql::catalog::definition_capture::types::domains,
        )?;
        self.enums = CatalogCell::capture_source(
            self.enums.snapshot(),
            Some(registry),
            uqa_sql::catalog::definition_capture::types::enums,
        )?;
        self.composites = CatalogCell::capture_source(
            self.composites.snapshot(),
            Some(registry),
            uqa_sql::catalog::definition_capture::types::composites,
        )?;
        self.ranges = CatalogCell::capture_source(
            self.ranges.snapshot(),
            Some(registry),
            uqa_sql::catalog::definition_capture::types::ranges,
        )?;
        self.shells = CatalogCell::capture_source(
            self.shells.snapshot(),
            Some(registry),
            uqa_sql::catalog::definition_capture::types::shells,
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
            uqa_sql::catalog::security::capture::schemas,
        )?;
        self.sequence_security = CatalogCell::capture_source(
            self.sequence_security.snapshot(),
            Some(registry),
            uqa_sql::catalog::security::capture::sequences,
        )?;
        self.roles = CatalogCell::capture_source(
            self.roles.snapshot(),
            Some(registry),
            uqa_sql::catalog::roles::capture::roles,
        )?;
        self.role_memberships = CatalogCell::capture_source(
            self.role_memberships.snapshot(),
            Some(registry),
            uqa_sql::catalog::roles::capture::memberships,
        )?;
        self.foreign_table_security = CatalogCell::capture_source(
            self.foreign_table_security.snapshot(),
            Some(registry),
            uqa_sql::catalog::security::capture::relations::tables,
        )?;
        self.builtin_routine_security = CatalogCell::capture_source(
            self.builtin_routine_security.snapshot(),
            Some(registry),
            uqa_sql::catalog::security::capture::relations::builtin_routines,
        )?;
        self.system_relation_security = CatalogCell::capture_source(
            self.system_relation_security.snapshot(),
            Some(registry),
            uqa_sql::catalog::security::capture::relations::system_relations,
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
            uqa_sql::catalog::definition_capture::identities::graph_oids,
        )?;
        self.catalog_indexes = CatalogCell::capture_resource(
            self.catalog_indexes.snapshot(),
            registry,
            uqa_storage::catalog::capture::indexes,
        )?;
        self.scoring_params = CatalogCell::capture_resource(
            self.scoring_params.snapshot(),
            registry,
            uqa_storage::catalog::capture::scoring_params,
        )?;
        Ok(())
    }
}
