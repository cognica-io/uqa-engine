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
        Ok(())
    }
}
