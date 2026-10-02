//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native row addressing over a retained common committed/private view.

mod graph;
mod graph_definitions;
mod graph_labels;
mod graph_observations;
mod graph_selection;
mod identity_presence;
mod latest;
mod latest_documents;
mod latest_index_entries;
mod latest_vertices;
mod private_rows;
pub(crate) use private_rows::PrivateRows;

use rusqlite::types::ValueRef;
use uqa_storage::mvcc::{DatabaseId, MergedRecordSnapshot, VersionError, VersionedKeyValueStore};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::{KeyValueBatch, KeyValueStore, StorageTransactionModel};

use super::{
    decode_record, invalid, owners, NativeRecord, NativeRecordFamily as Family,
    NativeRecordIdentity, NativeRecordOwner,
};
use crate::connection::Result;

pub(crate) struct NativeSnapshot {
    pub(crate) view: MergedRecordSnapshot,
    pub(crate) control: StorageReadControl,
    /// Stable native data namespace used to address records across history restorations.
    pub(crate) database: DatabaseId,
    /// Transaction history belongs to the logical provider and may differ after backup restoration.
    pub(crate) history: DatabaseId,
}

impl NativeSnapshot {
    pub(crate) fn record_read(&self) -> uqa_storage::mvcc::RecordRead<'_> {
        uqa_storage::mvcc::RecordRead::new(&self.view, self.history, &self.control)
    }

    /// Select entity identities without reading unselected property payloads. `None` names the catalog graph; a string selects one standalone namespace.
    pub(crate) fn visit_graph_ids(
        &self,
        scope: Option<&str>,
        filter: uqa_storage::GraphEntityFilter<'_>,
        after: Option<u64>,
        visit: impl FnMut(i64) -> Result<bool>,
    ) -> Result<()> {
        self.visit_graph_ids_with_page_size(scope, filter, after, 256, visit)
    }

    /// Bound selection work by a caller's requested page while retaining secondary-filter and tombstone continuation.
    pub(crate) fn visit_graph_ids_with_page_size(
        &self,
        scope: Option<&str>,
        filter: uqa_storage::GraphEntityFilter<'_>,
        after: Option<u64>,
        page_size: usize,
        visit: impl FnMut(i64) -> Result<bool>,
    ) -> Result<()> {
        graph_selection::visit(self, scope, filter, after, page_size.min(256), visit)
    }

    /// Release each physical read before visiting decoded rows, allowing callbacks to probe other records on this retained boundary.
    pub(crate) fn visit_paged_rows(
        &self,
        family: Family,
        components: &[ValueRef<'_>],
        visit: impl FnMut(&[ValueRef<'_>]) -> Result<bool>,
    ) -> Result<()> {
        self.visit_paged_owned_rows(
            family,
            NativeRecordOwner::Database(self.database),
            components,
            visit,
        )
    }

    pub(crate) fn visit_paged_owned_rows(
        &self,
        family: Family,
        owner: NativeRecordOwner,
        components: &[ValueRef<'_>],
        mut visit: impl FnMut(&[ValueRef<'_>]) -> Result<bool>,
    ) -> Result<()> {
        let prefix =
            NativeRecordIdentity::new(family, owner)?.encode_prefix(components, &self.control)?;
        let mut after = uqa_core::memory::BudgetedVec::new(self.control.memory());
        loop {
            let page = self.view.scan(
                &prefix,
                (!after.is_empty()).then_some(&*after),
                64,
                &self.control,
            )?;
            let Some(last) = page.last() else {
                return Ok(());
            };
            after.clear();
            after.extend_from_slice(&last.key)?;
            for entry in page.iter() {
                if let Some(value) = entry.record.value() {
                    let (_, row) = decode_record(&entry.key, value, &self.control)?;
                    if !visit(&row)? {
                        return Ok(());
                    }
                }
            }
        }
    }

    pub(crate) fn capture(store: &VersionedKeyValueStore, database: DatabaseId) -> Result<Self> {
        let StorageTransactionModel::VersionedConcurrent { database: history } =
            store.transaction_model()
        else {
            return Err(invalid("native snapshot requires versioned history").into());
        };
        Ok(Self {
            view: store.record_snapshot()?,
            control: store.retention_control(),
            database,
            history,
        })
    }

    pub(crate) fn read_row<R>(
        &self,
        family: Family,
        owner: NativeRecordOwner,
        components: &[ValueRef<'_>],
        read: impl FnOnce(&[ValueRef<'_>]) -> Result<R>,
    ) -> Result<Option<R>> {
        self.read_row_controlled(family, owner, components, &self.control, read)
    }

    pub(crate) fn read_row_controlled<R>(
        &self,
        family: Family,
        owner: NativeRecordOwner,
        components: &[ValueRef<'_>],
        control: &StorageReadControl,
        read: impl FnOnce(&[ValueRef<'_>]) -> Result<R>,
    ) -> Result<Option<R>> {
        self.control.check()?;
        control.check()?;
        let key = NativeRecordIdentity::new(family, owner)?.encode_key(components, control)?;
        let record = self.view.get(&key, control)?;
        self.control.check()?;
        control.check()?;
        let Some(bytes) = record.as_ref().and_then(|record| record.value()) else {
            return Ok(None);
        };
        let (_, row) = decode_record(&key, bytes, control)?;
        let result = read(&row)?;
        control.check()?;
        self.control.check()?;
        Ok(Some(result))
    }

    pub(crate) fn table_owner(&self, table: &str) -> Result<Option<NativeRecordOwner>> {
        self.table_owner_controlled(table, &self.control)
    }

    /// Decode one row from borrowed provider bytes. The internal decoder must not reenter persistence or invoke user callbacks.
    pub(crate) fn borrow_row_controlled<R>(
        &self,
        family: Family,
        owner: NativeRecordOwner,
        components: &[ValueRef<'_>],
        control: &StorageReadControl,
        read: impl FnOnce(&[ValueRef<'_>]) -> Result<R>,
    ) -> Result<Option<R>> {
        self.control.check()?;
        control.check()?;
        let key = NativeRecordIdentity::new(family, owner)?.encode_key(components, control)?;
        let mut read = Some(read);
        let mut result = None;
        self.view.visit_value(&key, control, &mut |record| {
            self.control.check()?;
            control.check()?;
            if let Some(bytes) = record.and_then(|record| record.value) {
                let (_, row) = decode_record(&key, bytes, control)?;
                result = Some(
                    read.take().expect("one visible record")(&row)
                        .map_err(|error| VersionError::Storage(error.into()))?,
                );
            }
            control.check()?;
            self.control.check()?;
            Ok(())
        })?;
        Ok(result)
    }

    pub(crate) fn table_owner_controlled(
        &self,
        table: &str,
        control: &StorageReadControl,
    ) -> Result<Option<NativeRecordOwner>> {
        Ok(self
            .table_binding_controlled(table, control)?
            .map(|(owner, _)| owner))
    }

    pub(crate) fn table_binding(&self, table: &str) -> Result<Option<(NativeRecordOwner, bool)>> {
        self.table_binding_controlled(table, &self.control)
    }

    /// The binding of `table` in this view. A binding this transaction did not change is the committed one, which the committed snapshot keeps after its first read.
    fn table_binding_controlled(
        &self,
        table: &str,
        control: &StorageReadControl,
    ) -> Result<Option<(NativeRecordOwner, bool)>> {
        let Some(committed) = self
            .view
            .committed()
            .provider_snapshot()
            .and_then(|snapshot| snapshot.downcast_ref::<crate::mvcc::read::Snapshot>())
        else {
            return self.read_table_binding(table, control);
        };
        self.control.check()?;
        control.check()?;
        let key = NativeRecordIdentity::new(
            Family::TableOwners,
            NativeRecordOwner::Database(self.database),
        )?
        .encode_key(&[ValueRef::Text(table.as_bytes())], control)?;
        if self.view.has_private_change(&key, control)? {
            return self.read_table_binding(table, control);
        }
        if let Some(binding) = committed.table_owners.get(table) {
            return Ok(binding);
        }
        let binding = self.read_table_binding(table, control)?;
        committed.table_owners.remember(table, binding);
        Ok(binding)
    }

    fn read_table_binding(
        &self,
        table: &str,
        control: &StorageReadControl,
    ) -> Result<Option<(NativeRecordOwner, bool)>> {
        self.read_row_controlled(
            Family::TableOwners,
            NativeRecordOwner::Database(self.database),
            &[ValueRef::Text(table.as_bytes())],
            control,
            |values| {
                let id = |value: ValueRef<'_>| {
                    value
                        .as_blob()
                        .ok()
                        .and_then(|bytes| bytes.try_into().ok())
                        .ok_or_else(|| invalid("native table owner must have a 16-byte identity"))
                };
                let owner = NativeRecordOwner::Object {
                    identity: id(values[1])?,
                    generation: id(values[2])?,
                };
                NativeRecordIdentity::new(Family::Documents, owner)?;
                Ok((owner, values[3] == ValueRef::Integer(1)))
            },
        )
    }

    pub(crate) fn allocate_identity(&self, value: [u8; 16]) -> Result<[u8; 16]> {
        self.control.check()?;
        owners::allocate(ValueRef::Blob(&value))
            .map_err(crate::mvcc::Error::into_version)
            .map_err(Into::into)
    }

    /// Visit live rows on this fixed boundary without retaining a corpus-sized payload collection. The callback must not reenter persistence.
    pub(crate) fn visit_rows(
        &self,
        family: Family,
        owner: Option<NativeRecordOwner>,
        components: &[ValueRef<'_>],
        visit: impl FnMut(&[ValueRef<'_>]) -> Result<()>,
    ) -> Result<()> {
        let prefix = match owner {
            Some(owner) => NativeRecordIdentity::new(family, owner)?
                .encode_prefix(components, &self.control)?,
            None if components.is_empty() => {
                NativeRecordIdentity::family_prefix(family, &self.control)?
            }
            None => return Err(invalid("native components require an owner").into()),
        };
        self.visit_row_prefix(&prefix, visit)
    }

    pub(crate) fn visit_object_rows(
        &self,
        family: Family,
        identity: [u8; 16],
        visit: impl FnMut(&[ValueRef<'_>]) -> Result<()>,
    ) -> Result<()> {
        let prefix = NativeRecordIdentity::object_prefix(family, identity, &self.control)?;
        self.visit_row_prefix(&prefix, visit)
    }

    /// Select a literal prefix within a single TEXT identity on this fixed view.
    pub(crate) fn visit_text_prefix_rows(
        &self,
        family: Family,
        owner: NativeRecordOwner,
        prefix: &str,
        visit: impl FnMut(&[ValueRef<'_>]) -> Result<()>,
    ) -> Result<()> {
        let prefix = NativeRecordIdentity::new(family, owner)?
            .encode_text_prefix(prefix.as_bytes(), &self.control)?;
        self.visit_row_prefix(&prefix, visit)
    }

    fn visit_row_prefix(
        &self,
        prefix: &[u8],
        mut visit: impl FnMut(&[ValueRef<'_>]) -> Result<()>,
    ) -> Result<()> {
        self.view.visit_prefix(
            prefix,
            None,
            usize::MAX,
            &self.control,
            &mut |key, record| {
                if let Some(bytes) = record.value {
                    let (_, row) = decode_record(key, bytes, &self.control)?;
                    visit(&row).map_err(|error| VersionError::Storage(error.into()))?;
                }
                Ok(true)
            },
        )?;
        Ok(())
    }

    pub(crate) fn contains_row(
        &self,
        family: Family,
        owner: NativeRecordOwner,
        components: &[ValueRef<'_>],
    ) -> Result<bool> {
        let key =
            NativeRecordIdentity::new(family, owner)?.encode_key(components, &self.control)?;
        Ok(self
            .view
            .metadata(&key, &self.control)?
            .is_some_and(|record| record.live))
    }

    /// Whether a definition row exists that every write of a table asks about. A row this transaction did not change is the committed one, whose presence the committed snapshot keeps after its first read.
    pub(crate) fn contains_definition_row(
        &self,
        family: Family,
        owner: NativeRecordOwner,
        components: &[ValueRef<'_>],
    ) -> Result<bool> {
        let key =
            NativeRecordIdentity::new(family, owner)?.encode_key(components, &self.control)?;
        let live = || -> Result<bool> {
            Ok(self
                .view
                .metadata(&key, &self.control)?
                .is_some_and(|record| record.live))
        };
        let Some(committed) = self
            .view
            .committed()
            .provider_snapshot()
            .and_then(|snapshot| snapshot.downcast_ref::<crate::mvcc::read::Snapshot>())
        else {
            return live();
        };
        if self.view.has_private_change(&key, &self.control)? {
            return live();
        }
        if let Some(live) = committed.row_presence.get(&key) {
            return Ok(live);
        }
        let live = live()?;
        committed.row_presence.remember(&key, live);
        Ok(live)
    }

    pub(crate) fn ensure_table_owner(
        &self,
        table: &str,
        batch: &mut dyn KeyValueBatch,
    ) -> Result<NativeRecordOwner> {
        if let Some(owner) = self.table_owner(table)? {
            return Ok(owner);
        }
        let allocate =
            || owners::allocate(ValueRef::Blob(&[])).map_err(crate::mvcc::Error::into_version);
        let identity = allocate()?;
        let generation = allocate()?;
        self.put_row(
            batch,
            Family::TableOwners,
            NativeRecordOwner::Database(self.database),
            &[
                ValueRef::Text(table.as_bytes()),
                ValueRef::Blob(&identity),
                ValueRef::Blob(&generation),
                ValueRef::Integer(0),
            ],
        )?;
        Ok(NativeRecordOwner::Object {
            identity,
            generation,
        })
    }

    pub(crate) fn put_row(
        &self,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        owner: NativeRecordOwner,
        row: &[ValueRef<'_>],
    ) -> Result<()> {
        let record = NativeRecord::encode(family, owner, row, &self.control)?;
        self.observe_graph_definition_put(batch, family, owner, row)?;
        self.observe_graph_labels_put(batch, family, owner, row)?;
        batch.put(record.key(), record.row())?;
        Ok(())
    }

    /// Stage a row at a key that never had a record, as its caller has established: the write then reads no earlier revision of the key.
    pub(crate) fn put_unused_row(
        &self,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        owner: NativeRecordOwner,
        row: &[ValueRef<'_>],
    ) -> Result<()> {
        let record = NativeRecord::encode(family, owner, row, &self.control)?;
        self.observe_graph_definition_put(batch, family, owner, row)?;
        self.observe_graph_labels_put(batch, family, owner, row)?;
        batch.put_unused(record.key(), record.row())?;
        Ok(())
    }

    pub(crate) fn delete_prefix(
        &self,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        owner: NativeRecordOwner,
        prefix: &[ValueRef<'_>],
    ) -> Result<()> {
        self.observe_graph_definition_delete(batch, family, owner, prefix)?;
        self.observe_graph_labels_delete(batch, family, owner, prefix)?;
        let prefix =
            NativeRecordIdentity::new(family, owner)?.encode_prefix(prefix, &self.control)?;
        if family == Family::VectorChanges {
            batch.delete_prefix_allow_absent(&prefix)?;
        } else {
            batch.delete_prefix(&prefix)?;
        }
        Ok(())
    }
}
