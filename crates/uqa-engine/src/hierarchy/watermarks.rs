//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable table generations and monotone counter publication for reconstruction.

use std::sync::Arc;

use uqa_core::{memory::RetainedValue, RelationIdentity};
use uqa_execution::schema::hierarchy::watermarks::{self, WatermarkTable, WatermarkTables};
use uqa_sql::{
    ast::{ColumnDef, TableHierarchy},
    catalog::table_definition::SharedTableDefinition,
};
use uqa_storage::{read_control::StorageReadControl, StorageBackendResult};

use crate::{state::TableRegistryMap, Engine, TableState};

#[derive(Clone)]
pub(crate) struct Table {
    state: Arc<TableState>,
    backend: Option<Arc<dyn uqa_storage::PersistentStorageBackend>>,
}

impl WatermarkTable for Table {
    fn columns(&self) -> SharedTableDefinition<Vec<ColumnDef>> {
        self.state.columns.snapshot()
    }

    fn hierarchy(&self) -> SharedTableDefinition<TableHierarchy> {
        self.state.hierarchy.snapshot()
    }

    fn watermark(&self) -> StorageBackendResult<u128> {
        let local = *self.state.next_id.read();
        self.state
            .identifier_allocator(self.backend.as_deref())?
            .next_id_floor(local)
    }

    fn raise_watermark(&self, minimum: u128) {
        let mut current = self.state.next_id.write();
        *current = (*current).max(minimum);
    }
}

struct Tables {
    values: RetainedValue<TableRegistryMap>,
    backend: Option<Arc<dyn uqa_storage::PersistentStorageBackend>>,
}

impl WatermarkTables for Tables {
    type Table = Table;

    fn tables(&self) -> impl Iterator<Item = (&RelationIdentity, Table)> + Send {
        self.values.iter().map(|(name, table)| {
            (
                name,
                Table {
                    state: Arc::clone(table),
                    backend: self.backend.clone(),
                },
            )
        })
    }
}

pub(crate) type Input = watermarks::WatermarkInput<Table>;
pub(crate) type Plan = watermarks::WatermarkPlan<Table>;

impl Engine {
    /// Rebuild the shared legacy counter only after the complete partition hierarchy is valid.
    pub(crate) fn synchronize_partition_identity_watermarks(&self) -> StorageBackendResult<()> {
        for update in watermarks::plan(&Tables {
            values: self.storage.tables.live_snapshot(),
            backend: self.storage.backend.clone(),
        })? {
            update.publish();
        }
        Ok(())
    }

    pub(crate) fn partition_watermark_input(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Input> {
        Input::new(
            Tables {
                values: self.storage.tables.live_snapshot(),
                backend: self.storage.backend.clone(),
            },
            control,
        )
    }
}
