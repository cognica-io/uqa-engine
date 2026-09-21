//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native row addressing over a retained common committed/private view.

mod graph;
mod graph_observations;
mod graph_selection;

use rusqlite::types::ValueRef;
use uqa_storage::mvcc::{DatabaseId, MergedRecordSnapshot, VersionError, VersionedKeyValueStore};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::KeyValueBatch;

use super::{
    decode_record, invalid, owners, NativeRecord, NativeRecordFamily as Family,
    NativeRecordIdentity, NativeRecordOwner,
};
use crate::connection::Result;

pub(crate) struct NativeSnapshot {
    pub(crate) view: MergedRecordSnapshot,
    pub(crate) control: StorageReadControl,
    pub(crate) database: DatabaseId,
}

impl NativeSnapshot {
    /// Select entity identities without reading unselected property payloads. `None` names the catalog graph; a string selects one standalone namespace.
    pub(crate) fn visit_graph_ids(
        &self,
        scope: Option<&str>,
        filter: uqa_storage::GraphEntityFilter<'_>,
        after: Option<u64>,
        visit: impl FnMut(i64) -> Result<bool>,
    ) -> Result<()> {
        graph_selection::visit(self, scope, filter, after, visit)
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
        Ok(Self {
            view: store.record_snapshot()?,
            control: store.retention_control(),
            database,
        })
    }

    pub(crate) fn read_row<R>(
        &self,
        family: Family,
        owner: NativeRecordOwner,
        components: &[ValueRef<'_>],
        read: impl FnOnce(&[ValueRef<'_>]) -> Result<R>,
    ) -> Result<Option<R>> {
        let key =
            NativeRecordIdentity::new(family, owner)?.encode_key(components, &self.control)?;
        let record = self.view.get(&key, &self.control)?;
        let Some(bytes) = record.as_ref().and_then(|record| record.value()) else {
            return Ok(None);
        };
        let (_, row) = decode_record(&key, bytes, &self.control)?;
        read(&row).map(Some)
    }

    pub(crate) fn table_owner(&self, table: &str) -> Result<Option<NativeRecordOwner>> {
        Ok(self.table_binding(table)?.map(|(owner, _)| owner))
    }

    pub(crate) fn table_binding(&self, table: &str) -> Result<Option<(NativeRecordOwner, bool)>> {
        self.read_row(
            Family::TableOwners,
            NativeRecordOwner::Database(self.database),
            &[ValueRef::Text(table.as_bytes())],
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
        batch.put(record.key(), record.row())?;
        Ok(())
    }

    pub(crate) fn delete_prefix(
        &self,
        batch: &mut dyn KeyValueBatch,
        family: Family,
        owner: NativeRecordOwner,
        prefix: &[ValueRef<'_>],
    ) -> Result<()> {
        batch.delete_prefix(
            &NativeRecordIdentity::new(family, owner)?.encode_prefix(prefix, &self.control)?,
        )?;
        Ok(())
    }
}
