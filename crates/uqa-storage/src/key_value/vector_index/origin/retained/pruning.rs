//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{append, invalid, Record, RetainedDiskANNCanonical, BYTES};
use crate::diskann_index::{
    catalog::DiskANNIndexResolver,
    changes::{DiskANNChangeJournal, DiskANNPruneRequest, DiskANNPruneResult},
    format::DiskANNChangeIdentity,
    DiskANNCanonicalRead,
};
use crate::key_value::{KeyValueDiskANNPruner, KeyValueRead};
use crate::{read_control::StorageReadControl, KeyValueBatch, StorageBackendResult};
use uqa_core::DocId;

impl RetainedDiskANNCanonical {
    /// Evaluate a bounded journal page in the caller's same read/batch scope. Only the catalog binding comes from this retained source; obsolescence uses the command's committed canonical view. Private canonical writes or a private head cannot authorize cleanup.
    pub fn prune_changes(
        &self,
        resolver: &dyn DiskANNIndexResolver,
        pruner: &KeyValueDiskANNPruner,
        mutation: (&dyn KeyValueRead, &mut dyn KeyValueBatch),
        request: DiskANNPruneRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNPruneResult> {
        let (read, batch) = mutation;
        self.require_current_index(read, batch, control)?;
        if read
            .revision(&[&self.vectors, &self.origins])?
            .has_private_changes()
        {
            return Err(invalid(
                "journal pruning requires committed canonical input",
            ));
        }
        pruner.require_selected(
            &self.index_scope(resolver, control)?,
            self.dimensions,
            self.index_parameters()
                .ok_or_else(|| invalid("missing index parameters"))?,
            read,
            batch,
            control,
        )?;
        let result = pruner.prune(
            &mut Journal {
                source: self,
                read,
                batch,
            },
            request,
            control,
        )?;
        self.check_control(control)?;
        Ok(result)
    }
}

struct Journal<'a> {
    source: &'a RetainedDiskANNCanonical,
    read: &'a dyn KeyValueRead,
    batch: &'a mut dyn KeyValueBatch,
}

impl DiskANNChangeJournal for Journal<'_> {
    fn next_after(
        &self,
        after: Option<DiskANNChangeIdentity>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        self.source.check_control(control)?;
        let cursor = after
            .map(|id| append(&self.source.changes, &id.encode(), control))
            .transpose()?;
        let mut selected = None;
        let mut failure = None;
        let result = self.read.visit_keys_after(
            &self.source.changes,
            cursor.as_deref(),
            1,
            control,
            &mut |key| {
                let result = (|| {
                    self.source.check_control(control)?;
                    if selected.is_some() || failure.is_some() {
                        return Err(invalid("pruning key cursor exceeded its page"));
                    }
                    let id = DiskANNChangeIdentity::decode(
                        key.strip_prefix(&*self.source.changes)
                            .ok_or_else(|| invalid("pruning key escaped its field"))?,
                    )?;
                    selected = Some(id);
                    Ok(())
                })();
                if let Err(error) = result {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                    return Err(invalid("pruning key consumer rejected data"));
                }
                Ok(())
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }
        result?;
        self.source.check_control(control)?;
        Ok(selected)
    }

    fn change(
        &self,
        identity: DiskANNChangeIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Record> {
        self.source.check_control(control)?;
        let key = append(&self.source.changes, &identity.encode(), control)?;
        if self
            .read
            .record_revision(&key)?
            .is_none_or(|revision| revision.has_private_changes())
        {
            return Err(invalid("pruning requires an actual committed change"));
        }
        let mut selected = None;
        let mut seen = false;
        let mut failure = None;
        let result = self
            .read
            .visit_value_bounded(&key, BYTES, control, &mut |value| {
                let result = (|| {
                    self.source.check_control(control)?;
                    if seen || failure.is_some() {
                        return Err(invalid("pruning change returned repeatedly"));
                    }
                    seen = true;
                    selected = Some(Record::decode(
                        value.ok_or_else(|| invalid("pruning change disappeared"))?,
                        self.source.dimensions,
                    )?);
                    Ok(())
                })();
                if let Err(error) = result {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                    return Err(invalid("pruning change consumer rejected data"));
                }
                Ok(())
            });
        if let Some(error) = failure {
            return Err(error);
        }
        result?;
        self.source.check_control(control)?;
        selected.ok_or_else(|| invalid("pruning change was not returned"))
    }

    fn current_origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Record>> {
        self.source.record_on(self.read, document, control)
    }

    fn remove(
        &mut self,
        identity: DiskANNChangeIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.source.check_control(control)?;
        let key = append(&self.source.changes, &identity.encode(), control)?;
        self.batch.require_unchanged(&key)?;
        self.batch.delete(&key)
    }
}
