//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep selected catalog generations independent of session locks during private restore.

use super::CatalogReadView;
use crate::state::TableRegistryMap;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MemoryBudget, RetainedValue},
    CancellationToken,
};
use uqa_execution::catalog::CatalogReadCapture;
use uqa_sql::SQLError;

pub(crate) struct RestoreCatalogSelection {
    pub(super) definitions: CatalogReadCapture,
    pub(super) tables: RetainedValue<TableRegistryMap>,
}

impl RestoreCatalogSelection {
    pub(crate) async fn read(
        self,
        memory: &MemoryBudget,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<CatalogReadView, SQLError> {
        self.definitions
            .capture_from_input(
                self.tables
                    .iter()
                    .map(crate::TableState::catalog_snapshot_entry),
                memory,
                original,
                input,
            )
            .await
    }
}
