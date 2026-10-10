//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;
use uqa_core::memory::{Budgeted, BudgetedVec};

use crate::mvcc::commit::RecordWriteKind;
use crate::mvcc::graph::OwnedGraphMutation;
use crate::mvcc::key::RecordKey;
use crate::mvcc::populations::OwnedPopulationMutation;
use crate::mvcc::serializable::OwnedPredicate;
use crate::mvcc::vector::{IndexKind, Key, OwnedVectorMutation};
use crate::mvcc::{SerializableTransactionId, SharedRecordValue, VersionError};
use crate::{KeyValueBatch, StorageBackendResult};

use super::transaction::Transaction;
use super::VersionedKeyValueStore;

mod records;

enum Operation {
    Requirement(BudgetedVec<u8>),
    ObservedRequirement(RecordKey, crate::mvcc::CommitSequence),
    ObservedRecord(RecordKey, SharedRecordValue, crate::mvcc::CommitSequence),
    RetainedRecord(
        RecordKey,
        SharedRecordValue,
        Arc<dyn crate::key_value::KeyValueRead + Send + Sync>,
    ),
    IdentifierObservation(BudgetedVec<u8>, u64),
    IdentifierInheritance(BudgetedVec<u8>, BudgetedVec<u8>),
    Records(records::Records),
    OccurrenceReset(BudgetedVec<u8>),
    Fence(BudgetedVec<u8>),
    Graph(OwnedGraphMutation),
    VectorInput(Budgeted<OwnedVectorMutation>),
    Population(OwnedPopulationMutation),
    VectorFence(IndexKind, BudgetedVec<u8>),
    /// A canonical record at a key that never had one.
    UnusedRecord(RecordKey, SharedRecordValue),
}

pub(super) struct Batch<'a> {
    store: &'a VersionedKeyValueStore,
    operations: BudgetedVec<Operation>,
    participant: Option<SerializableTransactionId>,
    writes: BudgetedVec<OwnedPredicate>,
    has_origins: bool,
}

impl<'a> Batch<'a> {
    pub(super) fn new(
        store: &'a VersionedKeyValueStore,
        participant: Option<SerializableTransactionId>,
    ) -> Self {
        Self {
            store,
            operations: BudgetedVec::new(store.control.memory()),
            participant,
            writes: BudgetedVec::new(store.control.memory()),
            has_origins: false,
        }
    }
    fn copy(&self, bytes: &[u8]) -> StorageBackendResult<BudgetedVec<u8>> {
        let mut owned = BudgetedVec::new(self.store.control.memory());
        owned.extend_from_slice(bytes)?;
        Ok(owned)
    }
    fn require_population_layout(&self) -> StorageBackendResult<()> {
        self.store
            .persistence
            .diskann_population_record_layout()
            .map(|_| ())
            .ok_or_else(|| {
                VersionError::InvalidEncoding("provider has no DiskANN population layout")
                    .into_storage_error()
            })
    }
    fn typed_record(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
        kind: RecordWriteKind,
    ) -> StorageBackendResult<()> {
        self.record_edit(key, value, kind, false)?;
        self.has_origins |= kind == RecordWriteKind::DiskANNOrigin;
        Ok(())
    }

    fn record_edit(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
        kind: RecordWriteKind,
        prefix: bool,
    ) -> StorageBackendResult<()> {
        if !matches!(self.operations.last(), Some(Operation::Records(_))) {
            // All record groups retain their prefixes against the first group's allowance.
            let memory = self
                .operations
                .iter()
                .rev()
                .find_map(|operation| match operation {
                    Operation::Records(records) => Some(records.budget().clone()),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    self.store
                        .control
                        .memory()
                        .child(self.store.control.memory().limit() / 32)
                });
            self.operations
                .push(Operation::Records(records::Records::new(&memory)))?;
        }
        let Some(Operation::Records(records)) = self.operations.last_mut() else {
            unreachable!()
        };
        records
            .push(key, value, kind, prefix, &self.store.control)
            .map_err(VersionError::into_storage_error)
    }

    pub(super) fn apply(&self, transaction: &mut Transaction) -> Result<(), VersionError> {
        let control = &self.store.control;
        transaction.writable()?;
        if self.participant
            != transaction
                .serializable_context()
                .map(crate::mvcc::SerializableReadContext::id)
        {
            return Err(VersionError::InvalidEncoding(
                "evaluated batch changed serializable participant",
            ));
        }
        let _readers = transaction.changes.read_scope(control)?;
        let population_view = (self.has_origins
            || self
                .operations
                .iter()
                .any(|operation| matches!(operation, Operation::Population(_))))
        .then(|| transaction.view())
        .transpose()?;
        for operation in self.operations.iter() {
            match operation {
                Operation::Requirement(key) => {
                    transaction.require_unchanged(key, control)?;
                }
                Operation::ObservedRequirement(key, expected) => {
                    transaction.require_observed(key, *expected, control)?;
                }
                Operation::ObservedRecord(key, value, expected) => {
                    transaction.write_observed(key, value, *expected, control)?;
                }
                Operation::RetainedRecord(key, value, source) => {
                    transaction.write_with_retained_source(key, value, source.clone(), control)?;
                }
                Operation::IdentifierObservation(_, _) | Operation::IdentifierInheritance(_, _) => {
                }
                Operation::Records(records) => records.visit(control, |edit| {
                    if edit.prefix {
                        transaction.delete_prefix_kind(edit.key.bytes(), edit.kind, control)?;
                    } else {
                        transaction.write_shared_record(
                            &edit.key,
                            edit.value.as_ref(),
                            edit.kind,
                            control,
                        )?;
                    }
                    Ok(())
                })?,
                Operation::OccurrenceReset(table) => {
                    let table = std::str::from_utf8(table)
                        .map_err(|_| VersionError::InvalidEncoding("invalid occurrence table"))?;
                    let key = crate::key_value::occurrence_records::guard(table, None, control)?;
                    transaction.replace(&key, Some(b""), control)?;
                    let key = crate::key_value::occurrence_records::format(table, control)?;
                    transaction.fence_record(&key, control)?;
                }
                Operation::Fence(key) => transaction.fence_record(key, control)?,
                Operation::Graph(mutation) => transaction.graph_mutation(mutation)?,
                Operation::Population(mutation) => transaction.population_mutation(mutation)?,
                Operation::VectorInput(mutation) => {
                    let guard = mutation.kind.layout(&*self.store.persistence)?.key(
                        &mutation.metadata,
                        Key::Document(mutation.document),
                        control,
                    )?;
                    transaction.fence_record(&guard, control)?;
                    transaction.vector_mutation(mutation, control)?;
                }
                Operation::VectorFence(kind, prefix) => {
                    self.fence_vector_structures(transaction, *kind, prefix)?;
                }
                Operation::UnusedRecord(key, value) => {
                    transaction.write_unused_record(key, value, control)?;
                }
            }
        }
        if let Some(before) = population_view {
            self.apply_populations(transaction, &before)?;
        }
        // Validate and stage every record first. Allocation uses persistence directly because the session's mutation boundary already holds its active-transaction lock.
        self.apply_identifiers()?;
        self.observe_writes(transaction)
    }

    /// Fence the structure guard of every live vector index whose metadata lies under `prefix`.
    fn fence_vector_structures(
        &self,
        transaction: &mut Transaction,
        kind: IndexKind,
        prefix: &[u8],
    ) -> Result<(), VersionError> {
        let control = &self.store.control;
        let view = transaction.view()?;
        let mut guards = BudgetedVec::new(control.memory());
        view.visit_keys(prefix, None, usize::MAX, control, &mut |key, record| {
            if record.live {
                let layout = kind.layout(&*self.store.persistence)?;
                let metadata =
                    layout
                        .metadata_key(key, control)?
                        .ok_or(VersionError::InvalidEncoding(
                            "invalid vector metadata prefix",
                        ))?;
                if &*metadata != key {
                    return Err(VersionError::InvalidEncoding(
                        "vector fence selected derived rows",
                    ));
                }
                let guard = layout.key(key, Key::Structure, control)?;
                guards.push(guard)?;
            }
            Ok(true)
        })?;
        for guard in guards.iter() {
            transaction.fence_record(guard, control)?;
        }
        Ok(())
    }

    fn apply_populations(
        &self,
        transaction: &mut Transaction,
        before: &crate::mvcc::MergedRecordSnapshot,
    ) -> Result<(), VersionError> {
        let control = &self.store.control;
        let origins = crate::mvcc::PrivateRecordChanges::new(control.memory());
        let mut lifecycle = BudgetedVec::new(control.memory());
        for operation in self.operations.iter() {
            control.check()?;
            match operation {
                Operation::Population(mutation) => lifecycle.push(mutation.clone())?,
                Operation::Records(records) => records.visit(control, |edit| {
                    if edit.kind == RecordWriteKind::DiskANNOrigin {
                        let expected = before
                            .metadata(edit.key.bytes(), control)?
                            .and_then(|row| row.revision);
                        let write = crate::mvcc::PreparedRecordWrite::from_shared(
                            edit.key.clone(),
                            expected,
                            edit.value.clone(),
                        )
                        .with_kind(RecordWriteKind::DiskANNOrigin);
                        origins.apply_owned(&[write], control)?;
                    }
                    Ok(())
                })?,
                _ => {}
            }
        }
        let origins = origins.prepare(control)?;
        let after = transaction.view()?;
        let generated = crate::mvcc::populations::stage(
            &origins,
            &lifecycle,
            before,
            &after,
            &*self.store.persistence,
            control,
        )?;
        let mut generated_writes = generated.writes();
        while let Some(write) = generated_writes.next(control)? {
            transaction.write_shared_record(
                &write.shared_key(),
                write.shared_value().as_ref(),
                RecordWriteKind::DiskANNPopulationPreview,
                control,
            )?;
        }
        Ok(())
    }

    fn apply_identifiers(&self) -> Result<(), VersionError> {
        let write_control = self.store.write_control();
        let observe = |namespace: &[u8], value: u64| {
            let allocation = self.store.persistence.allocate_identifiers(
                namespace,
                crate::mvcc::IdentifierRequest::Observe(value),
                &write_control,
            )?;
            self.store
                .observed
                .lock()
                .record(namespace, allocation.watermark());
            Ok::<_, VersionError>(allocation)
        };
        // An observation at or below a watermark this session has read raises nothing, so it needs no allocation.
        let raise = |namespace: &[u8], value: u64| {
            if self.store.observed.lock().covers(namespace, value) {
                return Ok(());
            }
            observe(namespace, value).map(|_| ())
        };
        // An observation only raises its namespace's watermark to the maximum observed value, so a run of observations of one namespace needs a single physical allocation. An inheritance reads its source watermark and ends the run.
        let mut run: Option<(&[u8], u64)> = None;
        for operation in self.operations.iter() {
            match operation {
                Operation::IdentifierObservation(namespace, value) => match &mut run {
                    Some((current, maximum)) if *current == &namespace[..] => {
                        *maximum = (*maximum).max(*value);
                    }
                    _ => {
                        if let Some((namespace, value)) = run.replace((namespace, *value)) {
                            raise(namespace, value)?;
                        }
                    }
                },
                Operation::IdentifierInheritance(from, to) => {
                    if let Some((namespace, value)) = run.take() {
                        raise(namespace, value)?;
                    }
                    // The source watermark must be the current one, which only an allocation reads.
                    let source = observe(from, 0)?;
                    raise(to, source.watermark())?;
                }
                _ => {}
            }
        }
        if let Some((namespace, value)) = run {
            raise(namespace, value)?;
        }
        Ok(())
    }

    fn observe_writes(&self, transaction: &Transaction) -> Result<(), VersionError> {
        if self.writes.is_empty() {
            return Ok(());
        }
        let context = transaction
            .serializable_context()
            .ok_or(VersionError::InvalidEncoding(
                "evaluated observation lost its serializable participant",
            ))?;
        let control = self.store.write_control();
        let mut mark = None;
        let result = context.with_graph(&control, |graph| {
            mark = Some(graph.write_mark(context.id())?);
            for predicate in self.writes.iter() {
                graph.observe_write(context.id(), predicate.borrowed(), &control)?;
            }
            Ok(())
        });
        if result.is_err() {
            if let Some(mark) = mark {
                // A checkpoint failure may be a lost reply after successful publication. Undo through a new admission with the original cleanup allowance, even when the invoking writer was cancelled.
                context.with_graph(&self.store.control, |graph| graph.rollback_writes(mark))?;
            }
        }
        result
    }
}

impl KeyValueBatch for Batch<'_> {
    fn serializable_participant(&self) -> Option<SerializableTransactionId> {
        self.participant
    }
    fn observe_serializable_write(
        &mut self,
        predicate: crate::mvcc::SerializablePredicate<'_>,
    ) -> StorageBackendResult<()> {
        if self.participant.is_none() {
            return Err(
                VersionError::InvalidEncoding("batch has no serializable participant")
                    .into_storage_error(),
            );
        }
        predicate
            .validate(true)
            .map_err(VersionError::into_storage_error)?;
        let owned = OwnedPredicate::new(predicate, self.store.control.memory())
            .map_err(VersionError::into_storage_error)?;
        self.writes.push(owned)?;
        Ok(())
    }
    fn require_unchanged(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(Operation::Requirement(self.copy(key)?))?;
        Ok(())
    }
    fn put_with_retained_source(
        &mut self,
        key: &[u8],
        value: &[u8],
        source: Arc<dyn crate::key_value::KeyValueRead + Send + Sync>,
    ) -> StorageBackendResult<()> {
        source.control().check()?;
        self.operations.push(Operation::RetainedRecord(
            RecordKey::new(key, self.store.control.memory())
                .map_err(VersionError::into_storage_error)?,
            Arc::new(self.copy(value)?),
            source,
        ))?;
        Ok(())
    }
    fn touch_marker(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.typed_record(key, Some(value), RecordWriteKind::Marker)
    }
    fn replace_statistics_maintenance(
        &mut self,
        key: &[u8],
        value: &[u8],
    ) -> StorageBackendResult<()> {
        self.typed_record(key, Some(value), RecordWriteKind::StatisticsMaintenance)
    }
    fn observe_identifier(&mut self, namespace: &[u8], value: u64) -> StorageBackendResult<()> {
        crate::mvcc::IdentifierRequest::Observe(value)
            .reserve_workspace(namespace, &self.store.control)
            .map_err(VersionError::into_storage_error)?;
        self.operations.push(Operation::IdentifierObservation(
            self.copy(namespace)?,
            value,
        ))?;
        Ok(())
    }
    fn inherit_identifiers(&mut self, from: &[u8], to: &[u8]) -> StorageBackendResult<()> {
        for namespace in [from, to] {
            crate::mvcc::IdentifierRequest::Observe(0)
                .reserve_workspace(namespace, &self.store.control)
                .map_err(VersionError::into_storage_error)?;
        }
        self.operations.push(Operation::IdentifierInheritance(
            self.copy(from)?,
            self.copy(to)?,
        ))?;
        Ok(())
    }
    fn ivf_mutation(
        &mut self,
        metadata: &[u8],
        mutation: crate::ivf_index::IVFMutation<'_>,
    ) -> StorageBackendResult<()> {
        self.operations.push(Operation::VectorInput(
            OwnedVectorMutation::retain(
                IndexKind::IVFIndex,
                metadata,
                mutation
                    .try_into()
                    .map_err(VersionError::into_storage_error)?,
                &self.store.control,
            )
            .map_err(VersionError::into_storage_error)?,
        ))?;
        Ok(())
    }
    fn preview_ivf_record(&mut self, key: &[u8], value: Option<&[u8]>) -> StorageBackendResult<()> {
        self.typed_record(key, value, RecordWriteKind::IVFPreview)
    }
    fn preview_ivf_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.record_edit(prefix, None, RecordWriteKind::IVFPreview, true)
    }
    fn fence_ivf_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.operations.push(Operation::VectorFence(
            IndexKind::IVFIndex,
            self.copy(prefix)?,
        ))?;
        Ok(())
    }
    fn hnsw_mutation(
        &mut self,
        metadata: &[u8],
        mutation: crate::hnsw_index::HNSWMutation<'_>,
    ) -> StorageBackendResult<()> {
        self.operations.push(Operation::VectorInput(
            OwnedVectorMutation::retain(
                IndexKind::HNSWIndex,
                metadata,
                mutation
                    .try_into()
                    .map_err(VersionError::into_storage_error)?,
                &self.store.control,
            )
            .map_err(VersionError::into_storage_error)?,
        ))?;
        Ok(())
    }
    fn preview_hnsw_record(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        self.typed_record(key, value, RecordWriteKind::HNSWPreview)
    }
    fn preview_hnsw_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.record_edit(prefix, None, RecordWriteKind::HNSWPreview, true)
    }
    fn fence_hnsw_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.operations.push(Operation::VectorFence(
            IndexKind::HNSWIndex,
            self.copy(prefix)?,
        ))?;
        Ok(())
    }
    fn put(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.typed_record(key, Some(value), RecordWriteKind::Canonical)
    }

    fn put_unused(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.operations.push(Operation::UnusedRecord(
            RecordKey::new(key, self.store.control.memory())
                .map_err(VersionError::into_storage_error)?,
            Arc::new(self.copy(value)?),
        ))?;
        Ok(())
    }

    fn replace_diskann_origin(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.require_population_layout()?;
        self.typed_record(key, Some(value), RecordWriteKind::DiskANNOrigin)
    }

    fn invalidate_diskann_origin(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        if self
            .store
            .persistence
            .diskann_population_record_layout()
            .is_some()
        {
            self.typed_record(key, None, RecordWriteKind::DiskANNOrigin)
        } else {
            self.delete(key)
        }
    }

    fn publish_diskann_population(
        &mut self,
        key: &[u8],
        template: &[u8],
        origins: crate::diskann_index::pages::DiskANNOriginReader,
    ) -> StorageBackendResult<()> {
        self.require_population_layout()?;
        self.operations
            .push(Operation::Population(OwnedPopulationMutation::Publish {
                key: RecordKey::new(key, self.store.control.memory())
                    .map_err(VersionError::into_storage_error)?,
                template: Arc::new(self.copy(template)?),
                origins,
            }))?;
        Ok(())
    }

    fn retire_diskann_population(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.require_population_layout()?;
        self.operations
            .push(Operation::Population(OwnedPopulationMutation::Retire {
                key: RecordKey::new(key, self.store.control.memory())
                    .map_err(VersionError::into_storage_error)?,
            }))?;
        Ok(())
    }
    fn require_observed(
        &mut self,
        key: &[u8],
        revision: &crate::key_value::KeyValueReadRevision,
    ) -> StorageBackendResult<()> {
        let expected = revision
            .observed_commit(self.store.persistence.database_id())
            .ok_or_else(|| {
                VersionError::InvalidEncoding("metadata revision is not committed in this database")
                    .into_storage_error()
            })?;
        self.operations.push(Operation::ObservedRequirement(
            RecordKey::new(key, self.store.control.memory())
                .map_err(VersionError::into_storage_error)?,
            expected,
        ))?;
        Ok(())
    }
    fn put_observed(
        &mut self,
        key: &[u8],
        value: &[u8],
        revision: &crate::key_value::KeyValueReadRevision,
    ) -> StorageBackendResult<()> {
        let expected = revision
            .observed_commit(self.store.persistence.database_id())
            .ok_or_else(|| {
                VersionError::InvalidEncoding("metadata revision is not committed in this database")
                    .into_storage_error()
            })?;
        self.operations.push(Operation::ObservedRecord(
            RecordKey::new(key, self.store.control.memory())
                .map_err(VersionError::into_storage_error)?,
            Arc::new(self.copy(value)?),
            expected,
        ))?;
        Ok(())
    }
    fn delete(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.typed_record(key, None, RecordWriteKind::Canonical)
    }
    fn delete_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.record_edit(prefix, None, RecordWriteKind::Canonical, true)
    }
    fn delete_prefix_allow_absent(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.record_edit(prefix, None, RecordWriteKind::IdempotentDelete, true)
    }
    fn replace_occurrence_record(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        self.typed_record(key, value, RecordWriteKind::Occurrence)
    }
    fn invalidate_occurrence_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.record_edit(prefix, None, RecordWriteKind::OccurrenceCache, true)
    }
    fn occurrence_document(&mut self, table: &str, document: u64) -> StorageBackendResult<()> {
        let key =
            crate::key_value::occurrence_records::guard(table, Some(document), &self.store.control)
                .map_err(VersionError::into_storage_error)?;
        self.put(&key, b"")
    }
    fn reset_occurrences(&mut self, table: &str) -> StorageBackendResult<()> {
        self.operations
            .push(Operation::OccurrenceReset(self.copy(table.as_bytes())?))?;
        Ok(())
    }
    fn fence_record(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.operations.push(Operation::Fence(self.copy(key)?))?;
        Ok(())
    }
    fn graph_mutation(
        &mut self,
        mutation: crate::mvcc::GraphMutation<'_>,
    ) -> StorageBackendResult<()> {
        self.operations.push(Operation::Graph(
            OwnedGraphMutation::retain(mutation, &self.store.control)
                .map_err(VersionError::into_storage_error)?,
        ))?;
        Ok(())
    }
    fn preview_graph_invalidation(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        self.typed_record(key, value, RecordWriteKind::GraphPreview)
    }
    fn replace_graph_cache(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        self.typed_record(key, value, RecordWriteKind::GraphCache)
    }
    fn commit(self: Box<Self>) -> StorageBackendResult<()> {
        self.store.write(|transaction| self.apply(transaction))
    }
}
