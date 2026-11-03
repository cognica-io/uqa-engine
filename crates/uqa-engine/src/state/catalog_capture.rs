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
}
