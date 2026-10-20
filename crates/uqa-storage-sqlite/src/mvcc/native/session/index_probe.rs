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
        let mut ids = BudgetedVec::new(self.control.memory());
        let mut failure = None;
        let supported = self.visit_index_equal(table, owner, field, probe, &mut |id| {
            if let Err(error) = ids.push(id) {
                failure = Some(error);
                false
            } else {
                true
            }
        })?;
        if let Some(error) = failure {
            return Err(error.into());
        }
        Ok(supported.then(|| crate::btree_index::probe::into_ids(ids)))
    }

    /// Merge ordered physical equality hits with private replacements without collecting either population. A declined probe never invokes `visit`.
    pub(crate) fn visit_index_equal(
        &self,
        table: &str,
        owner: NativeRecordOwner,
        field: ValueRef<'_>,
        probe: &EqualityProbe,
        visit: &mut dyn FnMut(DocId) -> bool,
    ) -> Result<bool> {
        let control = &self.control;
        control.check()?;
        let private = self.view.private_revision().is_some();
        if private
            && (!self.private_index_support_complete(owner, field)?
                || !self.private_equality_supported(owner, field, probe)?)
        {
            return Ok(false);
        }
        let entries = NativeRecordIdentity::new(Family::BtreeIndexEntries, owner)?;
        let visited = self.read_latest_committed(control, &mut |connection| {
            if !stores_alone(connection, table, owner, control)? {
                return Ok(None);
            }
            let mut private = private
                .then(|| PrivateRows::after(&self.view, entries, &[field], None, control))
                .transpose()?;
            let mut stopped = false;
            let supported = probe.visit(connection, table, field, control, &mut |id| {
                let mut replaced = false;
                if let Some(private) = &mut private {
                    while let Some(next) = private.peek() {
                        let next = crate::btree_index::decode_doc_id(next)?;
                        if next > id {
                            break;
                        }
                        replaced |= next == id;
                        if !visit_private_equal(private, probe, control, visit)? {
                            stopped = true;
                            return Ok(false);
                        }
                    }
                }
                if !replaced && !visit(id) {
                    stopped = true;
                }
                Ok(!stopped)
            })?;
            if supported && !stopped {
                if let Some(private) = &mut private {
                    while private.peek().is_some() {
                        if !visit_private_equal(private, probe, control, visit)? {
                            break;
                        }
                    }
                }
            }
            Ok(Some(supported))
        })?;
        Ok(visited == Some(true))
    }

    /// Validate private comparison domains before the first candidate is delivered. The second sequential pass merges their keys without a resident mask.
    fn private_equality_supported(
        &self,
        owner: NativeRecordOwner,
        field: ValueRef<'_>,
        probe: &EqualityProbe,
    ) -> Result<bool> {
        let mut rows = PrivateRows::after(
            &self.view,
            NativeRecordIdentity::new(Family::BtreeIndexEntries, owner)?,
            &[field],
            None,
            &self.control,
        )?;
        while rows.peek().is_some() {
            if rows.visit(&mut |row| {
                Ok(probe
                    .matches(&private_value(row)?, &self.control)?
                    .is_some())
            })? == Some(false)
            {
                return Ok(false);
            }
            rows.advance()?;
        }
        Ok(true)
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

fn private_value(row: &[ValueRef<'_>]) -> Result<uqa_core::Value> {
    let value = row[3]
        .as_str()
        .map_err(|_| SQLiteError::StorageBackend("native B-tree value must be text".into()))?;
    decode_value(value)
}

fn visit_private_equal(
    private: &mut PrivateRows<'_>,
    probe: &EqualityProbe,
    control: &uqa_storage::read_control::StorageReadControl,
    visit: &mut dyn FnMut(DocId) -> bool,
) -> Result<bool> {
    let id = crate::btree_index::decode_doc_id(private.peek().expect("selected private identity"))?;
    let more = private.visit(
        &mut |row| match probe.matches(&private_value(row)?, control)? {
            Some(true) => Ok(visit(id)),
            Some(false) => Ok(true),
            None => Err(SQLiteError::StorageBackend(
                "validated private index comparison changed".into(),
            )),
        },
    )?;
    private.advance()?;
    Ok(more != Some(false))
}
