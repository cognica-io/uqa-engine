//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{io::Read, path::Path};

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use super::super::temporary::{io_error, TemporaryRun};
use super::super::{invalid, DiskANNBuildSink, DiskANNBuildVisitor, DiskANNTemporaryBudget};
use crate::diskann_index::{
    format::{
        DiskANNCanonicalOrigin, DiskANNGeneration, DiskANNOriginEntry, DiskANNOriginLayout,
        DiskANNOriginSummary, ORIGIN_BATCH_DOCUMENTS, ORIGIN_ENTRY_BYTES,
    },
    pages::DiskANNRecordKey,
    DiskANNCanonicalRead,
};
use crate::{read_control::StorageReadControl, StorageBackendResult};

pub(super) struct Origins {
    file: TemporaryRun,
    documents: u64,
    hash: Sha256,
}

impl Origins {
    pub(super) fn new(
        directory: &Path,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Ok(Self {
            file: TemporaryRun::new(directory, temporary, control)?,
            documents: 0,
            hash: Sha256::new(),
        })
    }

    /// Capture complete document origins during the same traversal that supplies encrypted graph/side input. An explicit empty tensor still appends one origin.
    pub(super) fn capture(
        &mut self,
        source: &dyn DiskANNCanonicalRead,
        control: &StorageReadControl,
        visit: &mut DiskANNBuildVisitor<'_>,
    ) -> StorageBackendResult<()> {
        source.check_control(control)?;
        let mut after = None;
        while let Some(document) = source.next_document_after(after, control)? {
            source.check_control(control)?;
            if after.is_some_and(|last| last >= document) {
                return Err(invalid("canonical origin cursor did not advance"));
            }
            let mut count = 0_u64;
            let mut version = None;
            let mut rejected = false;
            let selected =
                source.visit_document(document, control, &mut |ordinal, current, raw| {
                    if rejected
                        || u64::from(ordinal) != count
                        || version.is_some_and(|previous| previous != current)
                    {
                        rejected = true;
                        return Err(invalid("canonical tensor ordinals or origins differ"));
                    }
                    // Keep failure sticky even if a source suppresses the consumer's error.
                    rejected = true;
                    visit(document, ordinal, current, raw)?;
                    count += 1;
                    version = Some(current);
                    rejected = false;
                    Ok(())
                })?;
            if rejected
                || selected.is_none()
                || version.is_some_and(|current| Some(current) != selected)
            {
                return Err(invalid(
                    "canonical source did not complete its tensor origin",
                ));
            }
            let entry = DiskANNOriginEntry::new(
                document,
                DiskANNCanonicalOrigin::new(
                    selected.expect("checked origin"),
                    source.dimensions(),
                    count,
                )?,
            );
            let documents = self
                .documents
                .checked_add(1)
                .ok_or_else(|| invalid("canonical document count overflow"))?;
            let bytes = entry.encode();
            self.file.append(&bytes, control)?;
            self.hash.update(bytes);
            self.documents = documents;
            after = Some(document);
        }
        source.check_control(control)
    }

    pub(super) fn summary(&self) -> DiskANNOriginSummary {
        DiskANNOriginSummary::new(self.documents, self.hash.clone().finalize().into())
            .expect("captured origin summary")
    }

    pub(super) fn check_record_limit(
        &self,
        generation: DiskANNGeneration,
        dimensions: u32,
        maximum: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        if self.documents != 0 {
            let layout = DiskANNOriginLayout::new(generation, dimensions, self.documents)?;
            control.check_value_size(layout.encoded_bytes(0)?, maximum)?;
        }
        Ok(())
    }

    pub(super) fn write(
        &self,
        generation: DiskANNGeneration,
        dimensions: u32,
        sink: &mut dyn DiskANNBuildSink,
        maximum: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let layout = DiskANNOriginLayout::new(generation, dimensions, self.documents)?;
        self.file.read(control, |file| {
            let mut entries = BudgetedVec::new(control.memory());
            let mut first = 0;
            let mut hash = Sha256::new();
            while first < self.documents {
                control.check()?;
                let count = (self.documents - first).min(ORIGIN_BATCH_DOCUMENTS as u64) as usize;
                entries.clear();
                entries.reserve(count)?;
                for _ in 0..count {
                    control.check()?;
                    let mut bytes = [0; ORIGIN_ENTRY_BYTES];
                    file.read_exact(&mut bytes).map_err(io_error)?;
                    hash.update(bytes);
                    entries.push(DiskANNOriginEntry::decode(&bytes, dimensions)?)?;
                }
                let bytes = layout.encode(first, &entries, control)?;
                control.check_value_size(bytes.len(), maximum)?;
                sink.write_record(DiskANNRecordKey::Origins(first), &bytes, maximum, control)?;
                first += count as u64;
            }
            if <[u8; 32]>::from(hash.finalize()) != self.summary().digest() {
                return Err(invalid("captured origin artifact changed"));
            }
            Ok(())
        })
    }
}
