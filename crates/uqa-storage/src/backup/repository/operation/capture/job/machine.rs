//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture scheduling retains exact unit, pack, parent and verification positions.

use super::super::super::{pack::CapturePack, PackObject, Preparation};
use super::super::{
    add,
    parent::{ParentInventory, ParentStep},
    Pending, Reference, UnitMetadata,
};
use super::{
    BackupCapturePhase, BackupCaptureProgress, BackupCaptureReport, BackupImageSource,
    BackupImageStatistics, BackupOperation, BackupResult, BackupSourceStatistics, FileBudget,
    InventorySummary, StorageReadControl,
};
use crate::{
    backup::{BackupError, BackupImageReader, BackupSourceStep, InventoryWriter},
    temporary_file::TemporaryFile,
};
use uqa_core::memory::BudgetedVec;

mod finish;
mod unit;
use unit::Unit;

#[derive(Clone, Copy)]
enum Stage {
    Parent,
    Select,
    Match,
    Compare,
    Write,
    UnitComplete,
    Pack,
    Entries,
    ParentEnd,
    Inventory,
    InventoryWrite,
    InventorySeal,
    Verify,
    Seal,
    Complete,
}
#[derive(Clone, Copy)]
enum AfterPack {
    Select,
    ParentEnd,
}

pub(super) struct Machine<'a, S> {
    source: Option<S>,
    source_statistics: BackupSourceStatistics,
    parent: Option<ParentInventory<'a>>,
    inventory: Option<InventoryWriter<TemporaryFile>>,
    files: FileBudget,
    control: StorageReadControl,
    stage: Stage,
    selected: bool,
    unit: Option<Unit>,
    pack: Option<CapturePack>,
    pending: BudgetedVec<Pending>,
    pending_position: usize,
    sealed: Option<PackObject>,
    after_pack: AfterPack,
    retained: usize,
    retention_limit: usize,
    decoded: u64,
    frame: BudgetedVec<u8>,
    filled: usize,
    statistics: BackupImageStatistics,
    summary: Option<InventorySummary>,
    expected: Option<InventorySummary>,
    inventory_copy: Option<TemporaryFile>,
    inventory_frames: u64,
    candidate: Option<Preparation>,
    verifier: Option<BackupImageReader<'a>>,
    verify_unit: bool,
    verified_object_bytes: u64,
}

impl<'a, S: BackupImageSource> Machine<'a, S> {
    pub(super) fn new(
        operation: &BackupOperation<'a>,
        source: S,
        files: &FileBudget,
        expected: Option<InventorySummary>,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.check()?;
        if source.cut() != operation.cut() {
            return Err(BackupError::Invalid(
                "image source differs from capture cut",
            ));
        }
        let repository = operation.repository;
        let parent = operation
            .parent_lease
            .as_ref()
            .map(|lease| ParentInventory::new(repository, lease.root(), control))
            .transpose()?;
        let spool = TemporaryFile::with_file_budget(files)?;
        let inventory =
            InventoryWriter::new(spool, operation.cut().format, repository.limits, control)?;
        let window = (16 * 1024)
            .min(repository.limits.max_frame_bytes() as usize)
            .min((control.memory().available() / 8).max(1));
        let mut frame = BudgetedVec::new(control.memory());
        frame.extend_from_slice(&[0_u8; 16 * 1024][..window])?;
        Ok(Self {
            source: Some(source),
            source_statistics: BackupSourceStatistics::default(),
            parent,
            inventory: Some(inventory),
            files: files.clone(),
            control: control.clone(),
            stage: Stage::Parent,
            selected: false,
            unit: None,
            pack: None,
            pending: BudgetedVec::new(control.memory()),
            pending_position: 0,
            sealed: None,
            after_pack: AfterPack::Select,
            retained: 0,
            retention_limit: 0,
            decoded: 0,
            frame,
            filled: 0,
            statistics: BackupImageStatistics::default(),
            summary: None,
            expected,
            inventory_copy: None,
            inventory_frames: 0,
            candidate: None,
            verifier: None,
            verify_unit: false,
            verified_object_bytes: 0,
        })
    }

    pub(super) fn progress(&self) -> BackupCaptureProgress {
        let mut image = self.statistics;
        if let Some(parent) = &self.parent {
            image.parent_units = parent.visited;
        }
        BackupCaptureProgress {
            phase: match self.stage {
                Stage::Verify | Stage::Seal => BackupCapturePhase::Verifying,
                Stage::Complete => BackupCapturePhase::Prepared,
                _ => BackupCapturePhase::Capturing,
            },
            source: self
                .source
                .as_ref()
                .map_or(self.source_statistics, BackupImageSource::statistics),
            image,
            verified_object_bytes: self.verified_object_bytes,
        }
    }

    pub(super) fn report(&self) -> BackupResult<BackupCaptureReport> {
        if !matches!(self.stage, Stage::Complete) {
            return Err(BackupError::Finished);
        }
        Ok(BackupCaptureReport {
            inventory: self.summary.ok_or(BackupError::Integrity)?,
            source: self.source_statistics,
            image: self.statistics,
        })
    }

    pub(super) fn advance(
        &mut self,
        operation: &mut BackupOperation<'a>,
        maximum_positions: usize,
        maximum_bytes: usize,
    ) -> BackupResult<bool> {
        self.control.check()?;
        if maximum_positions == 0 || maximum_bytes == 0 {
            return Err(BackupError::Invalid("zero capture work window"));
        }
        let bytes = maximum_bytes.min(self.frame.len());
        match self.stage {
            Stage::Parent => {
                if let Some(parent) = &mut self.parent {
                    if !matches!(parent.advance(bytes)?, ParentStep::Pending) {
                        self.stage = Stage::Select;
                    }
                } else {
                    self.stage = Stage::Select;
                }
            }
            Stage::Select => self.select(operation, maximum_positions.min(128))?,
            Stage::Match => self.match_parent(operation, bytes)?,
            Stage::Compare => self.compare(operation, bytes)?,
            Stage::Write => self.write_unit(operation, bytes)?,
            Stage::UnitComplete => self.complete_unit(operation)?,
            Stage::Pack => self.finish_pack(operation)?,
            Stage::Entries => self.append_entry(operation)?,
            Stage::ParentEnd => self.finish_parent(bytes)?,
            Stage::Inventory => self.finish_inventory(operation)?,
            Stage::InventoryWrite => self.write_inventory(operation, bytes)?,
            Stage::InventorySeal => self.seal_inventory(operation)?,
            Stage::Verify => self.verify(bytes)?,
            Stage::Seal => self.prepare(operation)?,
            Stage::Complete => return Ok(true),
        }
        Ok(matches!(self.stage, Stage::Complete))
    }

    fn select(
        &mut self,
        operation: &mut BackupOperation<'a>,
        maximum_positions: usize,
    ) -> BackupResult<()> {
        let source = self.source.as_mut().expect("source stage owns source");
        if !self.selected {
            match source.advance(maximum_positions)? {
                BackupSourceStep::Unit => self.selected = true,
                BackupSourceStep::Pending => (),
                BackupSourceStep::Complete => {
                    self.source_statistics = source.statistics();
                    self.source = None;
                    self.after_pack = AfterPack::ParentEnd;
                    self.stage = if self.pack.is_some() {
                        Stage::Pack
                    } else {
                        Stage::ParentEnd
                    };
                }
            }
            return Ok(());
        }
        let unit = source
            .current_unit()
            .ok_or(BackupError::Invalid("source has no selected image unit"))?;
        operation
            .repository
            .limits
            .check_key(unit.address.key.len())?;
        let charge = unit
            .address
            .key
            .len()
            .checked_add(size_of::<Pending>())
            .ok_or(BackupError::Invalid("image metadata size overflow"))?;
        if self.pack.is_some()
            && (self.pending.len() >= 32
                || self.decoded >= 64 * 1024
                || charge > self.retention_limit.saturating_sub(self.retained))
        {
            self.after_pack = AfterPack::Select;
            self.stage = Stage::Pack;
            return Ok(());
        }
        let mut key = BudgetedVec::new(self.control.memory());
        key.extend_from_slice(unit.address.key)?;
        self.unit = Some(Unit::new(UnitMetadata {
            namespace: unit.address.namespace,
            generation: unit.address.generation,
            family: unit.address.family,
            key,
            kind: unit.kind,
            bytes: unit.decoded_bytes,
        }));
        self.stage = Stage::Match;
        Ok(())
    }

    fn finish_pack(&mut self, operation: &mut BackupOperation<'a>) -> BackupResult<()> {
        let mut pack = self.pack.take().expect("pack stage owns output");
        let object = pack.finish(operation, &self.control)?;
        add(
            &mut self.statistics.written_object_bytes,
            object.encoded_bytes(),
        )?;
        self.sealed = Some(object);
        self.pending_position = 0;
        self.stage = Stage::Entries;
        Ok(())
    }

    fn append_entry(&mut self, operation: &BackupOperation<'a>) -> BackupResult<()> {
        if let Some(unit) = self.pending.get(self.pending_position) {
            let entry = unit.entry(
                self.sealed.expect("sealed pack"),
                operation.repository.limits,
                &self.control,
            )?;
            self.inventory
                .as_mut()
                .expect("capture inventory")
                .append(&entry)?;
            self.pending_position += 1;
        } else {
            self.pending = BudgetedVec::new(self.control.memory());
            self.sealed = None;
            self.retained = 0;
            self.decoded = 0;
            self.stage = match self.after_pack {
                AfterPack::Select => Stage::Select,
                AfterPack::ParentEnd => Stage::ParentEnd,
            };
        }
        Ok(())
    }

    fn finish_parent(&mut self, maximum_bytes: usize) -> BackupResult<()> {
        if let Some(parent) = &mut self.parent {
            if !matches!(parent.advance(maximum_bytes)?, ParentStep::Complete) {
                return Ok(());
            }
            self.statistics.parent_units = parent.visited;
        }
        self.parent = None;
        self.stage = Stage::Inventory;
        Ok(())
    }
}
