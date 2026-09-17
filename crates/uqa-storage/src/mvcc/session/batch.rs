//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::BudgetedVec;

use crate::mvcc::commit::RecordWriteKind;
use crate::mvcc::graph::OwnedGraphMutation;
use crate::mvcc::vector::{IndexKind, Key, OwnedVectorMutation};
use crate::mvcc::VersionError;
use crate::{KeyValueBatch, StorageBackendResult};

use super::transaction::Transaction;
use super::VersionedKeyValueStore;

enum Operation {
    Requirement(BudgetedVec<u8>),
    IdentifierObservation(BudgetedVec<u8>, u64),
    IdentifierInheritance(BudgetedVec<u8>, BudgetedVec<u8>),
    Put(BudgetedVec<u8>, BudgetedVec<u8>),
    Delete(BudgetedVec<u8>),
    DeletePrefix(BudgetedVec<u8>, RecordWriteKind),
    OccurrenceReset(BudgetedVec<u8>),
    Fence(BudgetedVec<u8>),
    Graph(OwnedGraphMutation),
    VectorInput(OwnedVectorMutation),
    VectorFence(IndexKind, BudgetedVec<u8>),
    TypedRecord {
        key: BudgetedVec<u8>,
        value: Option<BudgetedVec<u8>>,
        kind: RecordWriteKind,
    },
}

pub(super) struct Batch<'a> {
    store: &'a VersionedKeyValueStore,
    operations: BudgetedVec<Operation>,
}

impl<'a> Batch<'a> {
    pub(super) fn new(store: &'a VersionedKeyValueStore) -> Self {
        Self {
            store,
            operations: BudgetedVec::new(store.control.memory()),
        }
    }
    fn copy(&self, bytes: &[u8]) -> StorageBackendResult<BudgetedVec<u8>> {
        let mut owned = BudgetedVec::new(self.store.control.memory());
        owned.extend_from_slice(bytes)?;
        Ok(owned)
    }
    fn typed_record(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
        kind: RecordWriteKind,
    ) -> StorageBackendResult<()> {
        self.operations.push(Operation::TypedRecord {
            key: self.copy(key)?,
            value: value.map(|value| self.copy(value)).transpose()?,
            kind,
        })?;
        Ok(())
    }

    pub(super) fn apply(&self, transaction: &mut Transaction) -> Result<(), VersionError> {
        transaction.writable()?;
        for operation in self.operations.iter() {
            match operation {
                Operation::Requirement(key) => {
                    transaction.require_unchanged(key, &self.store.control)?;
                }
                Operation::IdentifierObservation(_, _) | Operation::IdentifierInheritance(_, _) => {
                }
                Operation::Put(key, value) => {
                    transaction.replace(key, Some(value), &self.store.control)?;
                }
                Operation::Delete(key) => transaction.replace(key, None, &self.store.control)?,
                Operation::DeletePrefix(prefix, kind) => {
                    transaction.delete_prefix_kind(prefix, *kind, &self.store.control)?;
                }
                Operation::OccurrenceReset(table) => {
                    let table = std::str::from_utf8(table)
                        .map_err(|_| VersionError::InvalidEncoding("invalid occurrence table"))?;
                    let key = crate::key_value::occurrence_records::guard(
                        table,
                        None,
                        &self.store.control,
                    )?;
                    transaction.replace(&key, Some(b""), &self.store.control)?;
                    let key =
                        crate::key_value::occurrence_records::format(table, &self.store.control)?;
                    transaction.fence_record(&key, &self.store.control)?;
                }
                Operation::Fence(key) => transaction.fence_record(key, &self.store.control)?,
                Operation::Graph(mutation) => transaction.graph_mutation(mutation)?,
                Operation::VectorInput(mutation) => {
                    let guard = mutation.kind.layout(&*self.store.persistence)?.key(
                        mutation.metadata.bytes(),
                        Key::Document(mutation.document),
                        &self.store.control,
                    )?;
                    transaction.fence_record(&guard, &self.store.control)?;
                    transaction.vector_mutation(mutation)?;
                }
                Operation::VectorFence(kind, prefix) => {
                    let view = transaction.view()?;
                    let mut guards = BudgetedVec::new(self.store.control.memory());
                    view.visit_keys(
                        prefix,
                        None,
                        usize::MAX,
                        &self.store.control,
                        &mut |key, record| {
                            if record.live {
                                let layout = kind.layout(&*self.store.persistence)?;
                                let metadata = layout
                                    .metadata_key(key, &self.store.control)?
                                    .ok_or(VersionError::InvalidEncoding(
                                        "invalid vector metadata prefix",
                                    ))?;
                                if &*metadata != key {
                                    return Err(VersionError::InvalidEncoding(
                                        "vector fence selected derived rows",
                                    ));
                                }
                                let guard = layout.key(key, Key::Structure, &self.store.control)?;
                                guards.push(guard)?;
                            }
                            Ok(true)
                        },
                    )?;
                    for guard in guards.iter() {
                        transaction.fence_record(guard, &self.store.control)?;
                    }
                }
                Operation::TypedRecord { key, value, kind } => {
                    transaction.write_record(key, value.as_deref(), *kind, &self.store.control)?;
                }
            }
        }
        // Validate and stage every record first. Allocation uses persistence directly because the session's mutation boundary already holds its active-transaction lock.
        for operation in self.operations.iter() {
            match operation {
                Operation::IdentifierObservation(namespace, value) => {
                    self.store.persistence.allocate_identifiers(
                        namespace,
                        crate::mvcc::IdentifierRequest::Observe(*value),
                        &self.store.control,
                    )?;
                }
                Operation::IdentifierInheritance(from, to) => {
                    let source = self.store.persistence.allocate_identifiers(
                        from,
                        crate::mvcc::IdentifierRequest::Observe(0),
                        &self.store.control,
                    )?;
                    self.store.persistence.allocate_identifiers(
                        to,
                        crate::mvcc::IdentifierRequest::Observe(source.watermark()),
                        &self.store.control,
                    )?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl KeyValueBatch for Batch<'_> {
    fn require_unchanged(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(Operation::Requirement(self.copy(key)?))?;
        Ok(())
    }
    fn touch_marker(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.typed_record(key, Some(value), RecordWriteKind::Marker)
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
        self.operations.push(Operation::DeletePrefix(
            self.copy(prefix)?,
            RecordWriteKind::IVFPreview,
        ))?;
        Ok(())
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
        self.operations.push(Operation::DeletePrefix(
            self.copy(prefix)?,
            RecordWriteKind::HNSWPreview,
        ))?;
        Ok(())
    }
    fn fence_hnsw_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.operations.push(Operation::VectorFence(
            IndexKind::HNSWIndex,
            self.copy(prefix)?,
        ))?;
        Ok(())
    }
    fn put(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(Operation::Put(self.copy(key)?, self.copy(value)?))?;
        Ok(())
    }
    fn delete(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.operations.push(Operation::Delete(self.copy(key)?))?;
        Ok(())
    }
    fn delete_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.operations.push(Operation::DeletePrefix(
            self.copy(prefix)?,
            RecordWriteKind::Canonical,
        ))?;
        Ok(())
    }
    fn replace_occurrence_record(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        self.typed_record(key, value, RecordWriteKind::Occurrence)
    }
    fn invalidate_occurrence_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.operations.push(Operation::DeletePrefix(
            self.copy(prefix)?,
            RecordWriteKind::OccurrenceCache,
        ))?;
        Ok(())
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
