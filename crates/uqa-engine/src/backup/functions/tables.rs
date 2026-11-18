//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql_error, StoredFunctionSite};
use crate::{
    state::{SnapshotTableCut, TableRegistryMap},
    SQLError,
};
use parking_lot::Mutex;
use uqa_core::{image_io::AsyncImageInput, memory::RetainedValue};
use uqa_execution::catalog::table_capture::visit_table_functions;
use uqa_sql::ast::RelationPersistence;
use uqa_storage::{
    backup::BackupSourceStatistics, read_control::StorageReadControl, StorageBackendResult,
};

pub(in crate::backup) enum Tables {
    Memory(SnapshotTableCut),
    Staged(RetainedValue<TableRegistryMap>),
}

impl Tables {
    pub(super) async fn visit(
        &self,
        source: &StorageReadControl,
        work: &mut AsyncImageInput<'_>,
        counts: &Mutex<BackupSourceStatistics>,
        visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
    ) -> StorageBackendResult<()> {
        match self {
            Self::Memory(cut) => {
                for table in cut.stored_expressions()? {
                    work.checkpoint().await?;
                    counts.lock().visited_positions += 1;
                    let (columns, checks) = table?;
                    visit_table_functions(&columns, &checks, source.cancellation(), work, visit)
                        .await
                        .map_err(sql_error)?;
                }
            }
            Self::Staged(tables) => {
                for table in tables.values() {
                    work.checkpoint().await?;
                    counts.lock().visited_positions += 1;
                    if table.persistence != RelationPersistence::Temporary {
                        let columns = table.columns.read().clone();
                        let checks = table.table_checks.read().clone();
                        visit_table_functions(
                            &columns,
                            &checks,
                            source.cancellation(),
                            work,
                            visit,
                        )
                        .await
                        .map_err(sql_error)?;
                    }
                }
            }
        }
        Ok(())
    }
}
