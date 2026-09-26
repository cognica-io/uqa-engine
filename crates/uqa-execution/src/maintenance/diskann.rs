//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One admitted journal pass per database, with finite catalog and change discovery.

#[cfg(test)]
mod tests;
mod transaction;

use std::{collections::BTreeMap, ops::Bound, sync::Arc};
use transaction::Job;
use uqa_core::memory::MemoryReservation;
use uqa_sql::schema::indexes::vectors::VectorIndexCatalog;
use uqa_storage::{
    diskann_index::{DiskANNIndexBinding, DiskANNIndexOptions},
    read_control::StorageReadControl,
    CatalogIndexRow, PersistentStorageBackend, RelationIdentity, StorageBackendError,
    StorageBackendResult,
};

/// Confirmed progress only. An evaluated but unresolved deletion never increments these counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiskANNMaintenanceStatus {
    pub completed_passes: u64,
    pub examined: u64,
    pub removed: u64,
    pub pending_completion: bool,
    pub last_error: Option<String>,
}

/// The host supplies coalesced wakes; this owner admits at most one pruning job and evaluates at most one 64-entry page per step. A retained catalog pass prevents new indexes from extending the current pass indefinitely.
pub struct DiskANNJournalMaintenance {
    indexes: Option<Arc<BTreeMap<RelationIdentity, CatalogIndexRow>>>,
    last_version: Option<u64>,
    after: Option<RelationIdentity>,
    name_memory: MemoryReservation,
    job: Option<Job>,
    control: StorageReadControl,
    status: DiskANNMaintenanceStatus,
    _memory: MemoryReservation,
}

impl DiskANNJournalMaintenance {
    pub fn new(control: &StorageReadControl) -> StorageBackendResult<Self> {
        control.check()?;
        Ok(Self {
            indexes: None,
            last_version: None,
            after: None,
            name_memory: control.memory().empty_reservation(),
            job: None,
            control: control.clone(),
            status: DiskANNMaintenanceStatus::default(),
            _memory: control.memory().reserve(size_of::<Self>())?,
        })
    }

    pub fn status(&self) -> DiskANNMaintenanceStatus {
        self.status.clone()
    }

    /// `version` is a commit generation already incorporated into the supplied catalog snapshot, never a newer independent observation. None disables unchanged-database coalescing. Commits during a finite pass trigger another pass instead of being consumed by its completion.
    pub fn step(
        &mut self,
        indexes: Arc<BTreeMap<RelationIdentity, CatalogIndexRow>>,
        version: Option<u64>,
        vectors: &dyn VectorIndexCatalog,
        backend: &dyn PersistentStorageBackend,
    ) -> StorageBackendResult<()> {
        let result = self.advance(indexes, version, vectors, backend);
        if result.is_err() {
            self.last_version = None;
        }
        self.status.pending_completion = self.job.as_ref().is_some_and(Job::pending);
        self.status.last_error = result.as_ref().err().map(ToString::to_string);
        result
    }

    fn advance(
        &mut self,
        indexes: Arc<BTreeMap<RelationIdentity, CatalogIndexRow>>,
        version: Option<u64>,
        vectors: &dyn VectorIndexCatalog,
        backend: &dyn PersistentStorageBackend,
    ) -> StorageBackendResult<()> {
        // Resolve original receipts even after cancellation; cancellation may forbid new work, never receipt acknowledgement.
        if let Some(job) = &mut self.job {
            let result = job.step(&self.control);
            if let Some(committed) = job.take_completed() {
                self.status.examined = self
                    .status
                    .examined
                    .saturating_add(committed.examined as u64);
                self.status.removed = self.status.removed.saturating_add(committed.removed as u64);
                if committed.next.is_none() {
                    self.status.completed_passes = self.status.completed_passes.saturating_add(1);
                }
            }
            if job.finished() {
                self.job = None;
            }
            return result;
        }
        self.control.check()?;
        if self.indexes.is_none() {
            if version.is_some() && version == self.last_version {
                return Ok(());
            }
            self.last_version = version;
        }
        let catalog = self.indexes.get_or_insert(indexes);
        let bound = self
            .after
            .as_ref()
            .map_or(Bound::Unbounded, Bound::Excluded);
        let Some((name, row)) = catalog.range((bound, Bound::Unbounded)).next() else {
            self.indexes = None;
            self.after = None;
            self.name_memory = self.control.memory().empty_reservation();
            return Ok(());
        };
        let memory = self.control.memory().reserve(
            name.schema
                .len()
                .checked_add(name.name.len())
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?,
        )?;
        self.after = Some(name.clone());
        self.name_memory = memory;
        if !row.index_type.eq_ignore_ascii_case("diskann") {
            return Ok(());
        }
        self.job = Some(admit(row, vectors, backend, &self.control)?);
        Ok(())
    }
}

fn admit(
    row: &CatalogIndexRow,
    vectors: &dyn VectorIndexCatalog,
    backend: &dyn PersistentStorageBackend,
    control: &StorageReadControl,
) -> StorageBackendResult<Job> {
    let bytes = [
        row.columns_json.len(),
        row.parameters_json.len(),
        row.definition_json.as_ref().map_or(0, String::len),
    ]
    .into_iter()
    .try_fold(0_usize, usize::checked_add)
    .and_then(|bytes| bytes.checked_mul(size_of::<uqa_sql::ast::Expr>() * 2))
    .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
    let _decoding = control.memory().reserve(bytes)?;
    let (field, dimensions, parameters) = crate::schema::indexes::diskann::target(vectors, row)?;
    let session = backend.open_controlled_session(control)?;
    session.validate_transaction_affinity()?;
    if session.backend.in_transaction()
        || session.backend.transaction_model() != backend.transaction_model()
        || session.backend.transaction_affinity() == backend.transaction_affinity()
        || session
            .backend
            .retention_control()
            .is_none_or(|retained| !retained.memory().shares_allowance(control.memory()))
        || session
            .backend
            .write_cancellation()
            .is_none_or(|cancellation| !cancellation.shares_signal(control.cancellation()))
    {
        return Err(StorageBackendError::Other(
            "DiskANN maintenance requires an independent inactive session with the same database, allowance and write cancellation"
                .into(),
        ));
    }
    let pruner = session.backend.diskann_journal_pruner(
        DiskANNIndexBinding {
            table: &row.table_name,
            field: &field,
            dimensions,
            index: &row.relation,
            resolver: Arc::new(crate::catalog::index::diskann::DiskANNIndexIdentityResolver),
            control,
        },
        DiskANNIndexOptions::for_parameters(parameters)
            .read
            .max_record_bytes,
    )?;
    Ok(Job::new(session.backend, pruner))
}
