//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    backup::{
        BackupCut, BackupError, BackupImageReader, BackupImageSource, BackupLease, BackupReadStep,
        BackupResult, BackupSourceStatistics, BackupSourceStep, BackupSourceUnit, InventorySummary,
    },
    read_control::StorageReadControl,
    temporary_file::FileBudget,
};

use super::{capture::workspace_error, BackupOperation, State};

/// Work performed by a complete standalone copy. Payload reads and whole-object verification are distinct passes; encoded output includes packs and inventory, excluding the publication descriptor. No counter estimates physical page I/O.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackupCopyReport {
    pub inventory: InventorySummary,
    pub copied_payload_bytes: u64,
    pub verified_source_object_bytes: u64,
    pub written_object_bytes: u64,
    /// Peak physical ciphertext workspace shared by source verification, inventory production and destination verification; excludes repository output and filesystem allocation metadata.
    pub workspace_peak_bytes: u64,
}

impl BackupOperation<'_> {
    /// Prepare a source-independent copy of one retained completed root in another repository. Begin this operation with the source root's exact cut and no parent. Every source frame, object, unit and inventory is verified; all destination bytes use its independently selected protection, fresh nonces and object identities. The logical image digest must match before a durable prepared descriptor exists. The source lease remains held throughout the copy; the resulting prepared operation publishes and resolves uncertain completion through the ordinary publication boundary, without the source or its key.
    pub fn prepare_copy(
        &mut self,
        source: &BackupLease<'_>,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<BackupCopyReport> {
        if !matches!(self.state, State::Capturing) {
            return Err(BackupError::Finished);
        }
        let result = self.copy_image(source, maximum_workspace_bytes, control);
        if result.is_err() {
            self.state = State::Failed;
        }
        result.map_err(workspace_error)
    }

    fn copy_image(
        &mut self,
        source: &BackupLease<'_>,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<BackupCopyReport> {
        control.check()?;
        let root = source.root();
        if self.repository.id == root.repository() {
            return Err(BackupError::Invalid(
                "standalone copy requires another repository",
            ));
        }
        if self.intent.parent.is_some() || self.cut() != root.cut() {
            return Err(BackupError::Invalid(
                "standalone copy intent differs from source",
            ));
        }
        let files = FileBudget::new(maximum_workspace_bytes);
        let reader = BackupImageReader::with_file_budget(source.repository, root, &files, control)?;
        let mut verified_source_object_bytes = 0;
        let image = CopySource {
            cut: root.cut(),
            reader,
            statistics: BackupSourceStatistics::default(),
            verified_source_object_bytes: &mut verified_source_object_bytes,
        };
        let report = self.capture_image(image, &files, Some(root.summary()), control)?;
        Ok(BackupCopyReport {
            inventory: report.inventory,
            copied_payload_bytes: report.source.copied_payload_bytes,
            verified_source_object_bytes,
            written_object_bytes: report.image.written_object_bytes,
            workspace_peak_bytes: files.usage().1,
        })
    }
}

struct CopySource<'a, 'b> {
    cut: BackupCut,
    reader: BackupImageReader<'a>,
    statistics: BackupSourceStatistics,
    verified_source_object_bytes: &'b mut u64,
}

impl BackupImageSource for CopySource<'_, '_> {
    fn cut(&self) -> BackupCut {
        self.cut
    }

    fn advance(&mut self, maximum_positions: usize) -> BackupResult<BackupSourceStep> {
        if maximum_positions == 0 {
            return Err(BackupError::Invalid("zero copy verification window"));
        }
        match self.reader.advance(16 * 1024)? {
            BackupReadStep::Unit => {
                self.statistics.visited_positions = self
                    .statistics
                    .visited_positions
                    .checked_add(1)
                    .ok_or(BackupError::Integrity)?;
                Ok(BackupSourceStep::Unit)
            }
            BackupReadStep::Pending => Ok(BackupSourceStep::Pending),
            BackupReadStep::Complete => {
                *self.verified_source_object_bytes = self.reader.verified_object_bytes();
                Ok(BackupSourceStep::Complete)
            }
        }
    }

    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        self.reader.current_unit().map(|entry| BackupSourceUnit {
            address: entry.address(),
            kind: entry.kind(),
            decoded_bytes: Some(entry.decoded_bytes()),
        })
    }

    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let bytes = self.reader.read_current(output)?;
        self.statistics.copied_payload_bytes = self
            .statistics
            .copied_payload_bytes
            .checked_add(bytes as u64)
            .ok_or(BackupError::Integrity)?;
        Ok(bytes)
    }

    fn statistics(&self) -> BackupSourceStatistics {
        self.statistics
    }
}
