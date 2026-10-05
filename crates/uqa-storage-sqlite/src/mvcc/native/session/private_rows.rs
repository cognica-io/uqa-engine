//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The session's private records of one native family and owner under a key prefix that a document identity completes, in document order, for a read of the stored rows to merge.
//!
//! A native key encodes its components after the owner prefix, and an integer component in signed order, so the private records under a prefix whose last missing component is the document identity are already in document order. Each selected entry retains its resident value or spill location until the row is requested.

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::{
    MergedRecordSnapshot, PrivateRecordCursor, PrivateRecordEntry, VersionError, VersionResult,
};
use uqa_storage::read_control::StorageReadControl;

use super::super::{decode_record, NativeRecordIdentity};
use crate::connection::Result;

/// A cursor over the private records of one family and owner under one key prefix.
pub(crate) struct PrivateRows<'a> {
    cursor: PrivateRecordCursor<'a>,
    current: Option<PrivateRecordEntry<'a>>,
    /// The position of the document identity among the key's components, after the prefix's.
    id_position: usize,
    control: &'a StorageReadControl,
    id: Option<i64>,
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
            cursor: view.private_cursor(&prefix, after.as_deref(), control)?,
            current: None,
            id_position: components.len(),
            control,
            id: None,
        };
        rows.advance()?;
        Ok(rows)
    }

    /// The document of the current record, or `None` after the last one.
    pub(crate) fn peek(&self) -> Option<i64> {
        self.id
    }

    pub(crate) fn advance(&mut self) -> VersionResult<()> {
        self.id = None;
        self.current = self.cursor.next(self.control)?;
        if let Some(entry) = &self.current {
            let mut id = None;
            NativeRecordIdentity::visit_key_components(
                entry.key(),
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
            self.id = Some(id.ok_or(VersionError::InvalidEncoding(
                "native document key lacks its identity",
            ))?);
        }
        Ok(())
    }

    /// Visit the current record as a row of its family's table, in its column order. Returns `None` for a deletion, which has no row, and otherwise what `visit` returned.
    pub(crate) fn visit(
        &self,
        visit: &mut dyn FnMut(&[ValueRef<'_>]) -> Result<bool>,
    ) -> VersionResult<Option<bool>> {
        let entry = self.current.as_ref().expect("a current private row");
        let write = entry.read(self.control)?;
        let Some(bytes) = write.value() else {
            return Ok(None);
        };
        let (_, row) = decode_record(entry.key(), bytes, self.control)?;
        let result = visit(&row).map_err(|error| VersionError::Storage(error.into()))?;
        self.control.check()?;
        Ok(Some(result))
    }

    /// Whether the current record holds a row, which a deletion does not.
    pub(crate) fn live(&self) -> VersionResult<bool> {
        self.control.check()?;
        Ok(self
            .current
            .as_ref()
            .expect("a current private row")
            .metadata()
            .live)
    }
}

#[cfg(test)]
mod tests;
