//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct value seeks over the latest projection with private replacements reconciled before publication.

use super::latest_documents::stores_alone;
use super::{Family, NativeRecordIdentity, NativeRecordOwner, NativeSnapshot, PrivateRows};
use crate::{
    btree_index::{decode_value, probe::EqualityProbe},
    connection::Result,
    SQLiteError,
};
use rusqlite::types::ValueRef;
use uqa_core::{memory::BudgetedVec, DocId};

impl NativeSnapshot {
    pub(crate) fn probe_index_equal(
        &self,
        table: &str,
        owner: NativeRecordOwner,
        field: ValueRef<'_>,
        probe: &EqualityProbe,
    ) -> Result<Option<Vec<DocId>>> {
        let control = &self.control;
        control.check()?;
        let Some(stored) = self.read_latest_committed(control, &mut |connection| {
            if !stores_alone(connection, table, owner, control)? {
                return Ok(None);
            }
            probe.read(connection, table, field, control)
        })?
        else {
            return Ok(None);
        };
        if self.view.private_revision().is_none() {
            return Ok(Some(crate::btree_index::probe::into_ids(stored)));
        }
        if !self.private_index_support_complete(owner, field)? {
            return Ok(None);
        }
        let entries = NativeRecordIdentity::new(Family::BtreeIndexEntries, owner)?;
        let mut result = BudgetedVec::new(control.memory());
        let mut position = 0;
        let mut private = PrivateRows::after(&self.view, entries, &[field], None, control)?;
        while let Some(id) = private.peek() {
            let id = DocId::try_from(id).map_err(|_| {
                SQLiteError::StorageBackend("negative index document identity".into())
            })?;
            // Merge one ordered private row at a time. Unrelated changes need no resident mask or key collection, even when the transaction itself has spilled.
            while position < stored.len() && stored[position] < id {
                result.push(stored[position])?;
                position += 1;
            }
            if stored.get(position) == Some(&id) {
                position += 1;
            }
            let mut supported = true;
            private.visit(&mut |row| {
                let value = row[3].as_str().map_err(|_| {
                    SQLiteError::StorageBackend("native B-tree value must be text".into())
                })?;
                match probe.matches(&decode_value(value)?, control)? {
                    Some(true) => result.push(id)?,
                    Some(false) => (),
                    None => supported = false,
                }
                Ok(true)
            })?;
            if !supported {
                return Ok(None);
            }
            private.advance()?;
        }
        result.extend_from_slice(&stored[position..])?;
        Ok(Some(crate::btree_index::probe::into_ids(result)))
    }

    /// Check only changed identities. The committed population certificate supplies completeness for every unchanged identity; metadata probes avoid decoding document bodies.
    fn private_index_support_complete(
        &self,
        owner: NativeRecordOwner,
        field: ValueRef<'_>,
    ) -> Result<bool> {
        for (source, target) in [
            (Family::Documents, Family::BtreeIndexEntries),
            (Family::BtreeIndexEntries, Family::Documents),
        ] {
            let prefix = [field];
            let prefix = if source == Family::Documents {
                &[][..]
            } else {
                &prefix[..]
            };
            let mut rows = PrivateRows::after(
                &self.view,
                NativeRecordIdentity::new(source, owner)?,
                prefix,
                None,
                &self.control,
            )?;
            while let Some(id) = rows.peek() {
                let key = [field, ValueRef::Integer(id)];
                let key = if target == Family::Documents {
                    &key[1..]
                } else {
                    &key[..]
                };
                if rows.live()? != self.contains_row(target, owner, key)? {
                    return Ok(false);
                }
                rows.advance()?;
            }
        }
        Ok(true)
    }
}
