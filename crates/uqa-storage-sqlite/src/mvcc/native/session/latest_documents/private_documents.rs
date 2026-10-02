//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The session's private document records of one table owner, in document order, for a read of the committed rows to merge.
//!
//! A native document key encodes the document identity after the owner prefix in signed integer order, so the private records under that prefix are already in document order, and the session's overlay holds their values in memory.

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::{MergedRecordSnapshot, PrivateRecordKey, VersionError};
use uqa_storage::read_control::StorageReadControl;

use super::super::super::{decode_record, NativeRecordIdentity};
use crate::connection::Result;
use crate::mvcc::PhysicalResult;

/// Private keys are read in pages of this many, so a transaction that changed many rows of the table holds one page of keys at a time.
const PAGE: usize = 64;

/// A cursor over the private document records of one owner.
pub(super) struct PrivateDocuments<'a> {
    view: &'a MergedRecordSnapshot,
    prefix: &'a [u8],
    control: &'a StorageReadControl,
    keys: BudgetedVec<PrivateRecordKey>,
    /// The document of each key in `keys`.
    ids: BudgetedVec<i64>,
    position: usize,
}

impl<'a> PrivateDocuments<'a> {
    /// The private records under the document `prefix` of `identity`'s owner after document `after`, or from the first.
    pub(super) fn after(
        view: &'a MergedRecordSnapshot,
        identity: NativeRecordIdentity,
        prefix: &'a [u8],
        after: Option<i64>,
        control: &'a StorageReadControl,
    ) -> PhysicalResult<Self> {
        let after = after
            .map(|id| identity.encode_key(&[ValueRef::Integer(id)], control))
            .transpose()?;
        let mut documents = Self {
            view,
            prefix,
            control,
            keys: BudgetedVec::new(control.memory()),
            ids: BudgetedVec::new(control.memory()),
            position: 0,
        };
        documents.load(after.as_deref())?;
        Ok(documents)
    }

    /// The document of the current record, or `None` after the last one.
    pub(super) fn peek(&self) -> Option<i64> {
        self.ids.get(self.position).copied()
    }

    pub(super) fn advance(&mut self) -> PhysicalResult<()> {
        self.position += 1;
        if self.position == PAGE {
            let mut last = BudgetedVec::new(self.control.memory());
            last.extend_from_slice(self.keys[PAGE - 1].key())
                .map_err(VersionError::from)?;
            self.load(Some(&last))?;
        }
        Ok(())
    }

    fn load(&mut self, after: Option<&[u8]>) -> PhysicalResult<()> {
        self.keys = self
            .view
            .private_keys(self.prefix, after, PAGE, self.control)?;
        self.ids.clear();
        for key in self.keys.iter() {
            let mut id = None;
            NativeRecordIdentity::visit_key_components(key.key(), self.control, |_, value| {
                id = Some(value.as_i64().map_err(|_| {
                    VersionError::InvalidEncoding("native document key must be integer")
                })?);
                Ok(())
            })?;
            self.ids
                .push(id.ok_or(VersionError::InvalidEncoding(
                    "native document key lacks its identity",
                ))?)
                .map_err(VersionError::from)?;
        }
        self.position = 0;
        Ok(())
    }

    /// Visit the current record as a `_documents` row, in its column order. Returns `None` for a deletion, which has no row, and otherwise what `visit` returned.
    pub(super) fn visit(
        &self,
        visit: &mut dyn FnMut(&[ValueRef<'_>]) -> Result<bool>,
    ) -> PhysicalResult<Option<bool>> {
        let key = self.keys[self.position].key();
        let mut visited = None;
        self.view.visit_value(key, self.control, &mut |record| {
            let Some(bytes) = record.and_then(|record| record.value) else {
                return Ok(());
            };
            let (_, row) = decode_record(key, bytes, self.control)?;
            visited = Some(visit(&row).map_err(|error| VersionError::Storage(error.into()))?);
            Ok(())
        })?;
        Ok(visited)
    }

    /// Whether the current record holds a document, which a deletion does not.
    pub(super) fn live(&self) -> PhysicalResult<bool> {
        let mut live = false;
        self.view.visit_value(
            self.keys[self.position].key(),
            self.control,
            &mut |record| {
                live = record.is_some_and(|record| record.value.is_some());
                Ok(())
            },
        )?;
        Ok(live)
    }
}
