//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The session's private records of one native family and owner under a key prefix that a document identity completes, in document order, for a read of the stored rows to merge.
//!
//! A native key encodes its components after the owner prefix, and an integer component in signed order, so the private records under a prefix whose last missing component is the document identity are already in document order. The session's overlay holds their values in memory.

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::{MergedRecordSnapshot, PrivateRecordKey, VersionError, VersionResult};
use uqa_storage::read_control::StorageReadControl;

use super::super::{decode_record, NativeRecordIdentity};
use crate::connection::Result;

/// Private keys are read in pages of this many, so a transaction that changed many rows of the table holds one page of keys at a time.
const PAGE: usize = 64;

/// A cursor over the private records of one family and owner under one key prefix.
pub(crate) struct PrivateRows<'a> {
    view: &'a MergedRecordSnapshot,
    prefix: BudgetedVec<u8>,
    /// The position of the document identity among the key's components, after the prefix's.
    id_position: usize,
    control: &'a StorageReadControl,
    keys: BudgetedVec<PrivateRecordKey>,
    /// The document of each key in `keys`.
    ids: BudgetedVec<i64>,
    position: usize,
}

impl<'a> PrivateRows<'a> {
    /// The private records of `identity`'s family and owner whose keys begin with `components` and end with a document identity, after document `after` or from the first.
    pub(crate) fn after(
        view: &'a MergedRecordSnapshot,
        identity: NativeRecordIdentity,
        components: &[ValueRef<'_>],
        after: Option<i64>,
        control: &'a StorageReadControl,
    ) -> VersionResult<Self> {
        let prefix = identity.encode_prefix(components, control)?;
        let after = after
            .map(|id| {
                let mut key = BudgetedVec::new(control.memory());
                key.extend_from_slice(components)?;
                key.push(ValueRef::Integer(id))?;
                identity.encode_key(&key, control)
            })
            .transpose()?;
        let mut rows = Self {
            view,
            prefix,
            id_position: components.len(),
            control,
            keys: BudgetedVec::new(control.memory()),
            ids: BudgetedVec::new(control.memory()),
            position: 0,
        };
        rows.load(after.as_deref())?;
        Ok(rows)
    }

    /// The document of the current record, or `None` after the last one.
    pub(crate) fn peek(&self) -> Option<i64> {
        self.ids.get(self.position).copied()
    }

    pub(crate) fn advance(&mut self) -> VersionResult<()> {
        self.position += 1;
        if self.position == PAGE {
            let mut last = BudgetedVec::new(self.control.memory());
            last.extend_from_slice(self.keys[PAGE - 1].key())?;
            self.load(Some(&last))?;
        }
        Ok(())
    }

    fn load(&mut self, after: Option<&[u8]>) -> VersionResult<()> {
        self.keys = self
            .view
            .private_keys(&self.prefix, after, PAGE, self.control)?;
        self.ids.clear();
        for key in self.keys.iter() {
            let mut id = None;
            NativeRecordIdentity::visit_key_components(
                key.key(),
                self.control,
                |position, value| {
                    if position == self.id_position {
                        id = Some(value.as_i64().map_err(|_| {
                            VersionError::InvalidEncoding(
                                "native document identity must be integer",
                            )
                        })?);
                    }
                    Ok(())
                },
            )?;
            self.ids.push(id.ok_or(VersionError::InvalidEncoding(
                "native document key lacks its identity",
            ))?)?;
        }
        self.position = 0;
        Ok(())
    }

    /// Visit the current record as a row of its family's table, in its column order. Returns `None` for a deletion, which has no row, and otherwise what `visit` returned.
    pub(crate) fn visit(
        &self,
        visit: &mut dyn FnMut(&[ValueRef<'_>]) -> Result<bool>,
    ) -> VersionResult<Option<bool>> {
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

    /// Whether the current record holds a row, which a deletion does not.
    pub(crate) fn live(&self) -> VersionResult<bool> {
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
