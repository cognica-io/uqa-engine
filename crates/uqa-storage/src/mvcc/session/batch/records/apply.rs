//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private conditions take precedence; consecutive committed conditions share one provider window. Prefix edits close that window before their ordered scans, and no future edit is retained or read before the current one succeeds.

use super::super::Transaction;
use super::{Edit, Records, StorageReadControl, VersionResult};
use crate::mvcc::{RecordMetadata, RecordMetadataRequests};

impl Records {
    pub(in crate::mvcc::session::batch) fn apply(
        &self,
        transaction: &mut Transaction,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        let committed = transaction.committed_snapshot();
        let mut writes = Writes {
            transaction,
            control,
            edits: self.cursor(control),
            pending: None,
        };
        loop {
            committed.visit_metadata(&mut writes, control)?;
            let Some(edit) = writes.pending.take() else {
                return Ok(());
            };
            debug_assert!(edit.prefix);
            writes
                .transaction
                .delete_prefix_kind(edit.key.bytes(), edit.kind, control)?;
        }
    }
}

struct Writes<'a, I> {
    transaction: &'a mut Transaction,
    control: &'a StorageReadControl,
    edits: I,
    pending: Option<Edit>,
}

impl<I: Iterator<Item = VersionResult<Edit>>> RecordMetadataRequests for Writes<'_, I> {
    fn advance(&mut self) -> VersionResult<bool> {
        loop {
            self.control.check()?;
            if self.pending.is_none() {
                self.pending = self.edits.next().transpose()?;
            }
            let Some(edit) = self.pending.as_ref() else {
                return Ok(false);
            };
            if edit.prefix {
                return Ok(false);
            }
            self.transaction.writable()?;
            let Some((metadata, previous)) = self
                .transaction
                .changes
                .write_metadata(edit.key.bytes(), self.control)?
            else {
                return Ok(true);
            };
            let edit = self.pending.take().expect("selected private edit");
            let condition = Transaction::condition_from_metadata(
                Some(metadata),
                Some(previous),
                edit.value.is_none(),
                edit.kind,
            );
            self.transaction.apply_shared_record(
                &edit.key,
                edit.value.as_ref(),
                condition,
                self.control,
            )?;
        }
    }

    fn key(&self) -> &[u8] {
        self.pending
            .as_ref()
            .expect("requested committed condition")
            .key
            .bytes()
    }

    fn accept(&mut self, metadata: Option<RecordMetadata>) -> VersionResult<()> {
        let edit = self.pending.take().expect("requested committed condition");
        let condition =
            Transaction::condition_from_metadata(metadata, None, edit.value.is_none(), edit.kind);
        self.transaction.apply_shared_record(
            &edit.key,
            edit.value.as_ref(),
            condition,
            self.control,
        )
    }
}
