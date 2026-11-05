//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One capture owner admits scheduling, failure, cancellation and final publication.

use super::{
    workspace_error, BackupCaptureReport, BackupImageSource, BackupImageStatistics,
    BackupOperation, BackupResult, BackupSourceStatistics, FileBudget, InventorySummary, State,
    StorageReadControl,
};
use crate::backup::{BackupCut, BackupError, BackupId, BackupRoot, LocalRepository};
use uqa_core::memory::MemoryReservation;

mod machine;
use machine::Machine;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupCapturePhase {
    Capturing,
    Verifying,
    Prepared,
    Complete,
    Cancelling,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupCaptureStep {
    Pending,
    Prepared,
    Complete,
}

/// Actual source and repository work. Physical page traffic and elapsed-time acceptance are not inferred from these counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackupCaptureProgress {
    pub phase: BackupCapturePhase,
    pub source: BackupSourceStatistics,
    pub image: BackupImageStatistics,
    pub verified_object_bytes: u64,
}

/// An admitted image job owns its source cut and repository operation. Each advance performs one bounded source, metadata, frame or verification step; publication remains explicit. Failure and cancellation release the source and encrypted workspace before another operation can proceed. Dropping the handle releases its leases; explicit cancellation records the durable outcome.
pub struct BackupImageCapture<'a, S> {
    machine: Option<Machine<'a, S>>,
    operation: BackupOperation<'a>,
    control: StorageReadControl,
    progress: BackupCaptureProgress,
    report: Option<BackupCaptureReport>,
    _memory: MemoryReservation,
}

/// A failed or unwinding step retires its source and workspace at this owner boundary.
struct Advance<'s, 'a, S> {
    capture: &'s mut BackupImageCapture<'a, S>,
    succeeded: bool,
}

impl<S> Drop for Advance<'_, '_, S> {
    fn drop(&mut self) {
        if !self.succeeded {
            self.capture.operation.state = State::Failed;
            self.capture.machine = None;
        }
    }
}

impl LocalRepository {
    pub fn begin_image_backup<S: BackupImageSource>(
        &self,
        source: S,
        parent: Option<BackupId>,
        maximum_encoded_bytes: u64,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<BackupImageCapture<'_, S>> {
        control.check()?;
        let memory = control
            .memory()
            .reserve(size_of::<BackupImageCapture<'_, S>>())?;
        let mut operation =
            self.begin_backup(source.cut(), parent, maximum_encoded_bytes, control)?;
        operation.state = State::Imaging;
        let machine = Machine::new(
            &operation,
            source,
            &FileBudget::new(maximum_workspace_bytes),
            None,
            control,
        )
        .map_err(workspace_error)?;
        let progress = machine.progress();
        Ok(BackupImageCapture {
            machine: Some(machine),
            operation,
            control: control.clone(),
            progress,
            report: None,
            _memory: memory,
        })
    }
}

impl<S: BackupImageSource> BackupImageCapture<'_, S> {
    pub const fn id(&self) -> BackupId {
        self.operation.id()
    }
    pub const fn cut(&self) -> BackupCut {
        self.operation.cut()
    }
    pub const fn report(&self) -> Option<BackupCaptureReport> {
        self.report
    }
    pub fn completed(&self) -> Option<BackupRoot> {
        match self.operation.state {
            State::Complete(root) => Some(root),
            _ => None,
        }
    }
    pub fn progress(&self) -> BackupCaptureProgress {
        let mut progress = self.progress;
        progress.phase = match self.operation.state {
            State::Capturing | State::Imaging => progress.phase,
            State::Prepared(_) => BackupCapturePhase::Prepared,
            State::Complete(_) => BackupCapturePhase::Complete,
            State::Cancelling => BackupCapturePhase::Cancelling,
            State::Cancelled => BackupCapturePhase::Cancelled,
            State::Failed => BackupCapturePhase::Failed,
        };
        progress
    }

    /// Advance at most one source window (at most 128 positions), payload read (at most 16 KiB), parent entry, output frame, or verification step. Configured frame/key limits also bound metadata and frame decoding. Filesystem synchronization can wait independently of these byte/work bounds. Both arguments must be nonzero. A completed preparation or publication is idempotent.
    pub fn advance(
        &mut self,
        maximum_positions: usize,
        maximum_bytes: usize,
    ) -> BackupResult<BackupCaptureStep> {
        match self.operation.state {
            State::Prepared(_) => return Ok(BackupCaptureStep::Prepared),
            State::Complete(_) => return Ok(BackupCaptureStep::Complete),
            State::Imaging => (),
            _ => return Err(BackupError::Finished),
        }
        let mut step = Advance {
            capture: self,
            succeeded: false,
        };
        let result = step.capture.advance_inner(maximum_positions, maximum_bytes);
        step.succeeded = result.is_ok();
        result.map_err(workspace_error)
    }

    fn advance_inner(
        &mut self,
        maximum_positions: usize,
        maximum_bytes: usize,
    ) -> BackupResult<BackupCaptureStep> {
        let machine = self.machine.as_mut().expect("active image owns machine");
        let result = machine.advance(&mut self.operation, maximum_positions, maximum_bytes);
        self.progress = machine.progress();
        match result {
            Ok(true) => {
                self.report = Some(machine.report()?);
                self.machine = None;
                Ok(BackupCaptureStep::Prepared)
            }
            Ok(false) => Ok(BackupCaptureStep::Pending),
            Err(error) => Err(error),
        }
    }

    /// Publish the fully verified root or resolve its earlier publication. No source access is required after preparation.
    pub fn publish(&mut self) -> BackupResult<BackupRoot> {
        self.operation.publish(&self.control)
    }

    /// Retire source/workspace ownership before recording cancellation; an already published root remains completed.
    pub fn cancel(&mut self) -> BackupResult<Option<BackupRoot>> {
        self.machine = None;
        self.operation.cancel()
    }
}

pub(super) fn run<S: BackupImageSource>(
    operation: &mut BackupOperation<'_>,
    source: S,
    files: &FileBudget,
    expected: Option<InventorySummary>,
    control: &StorageReadControl,
) -> BackupResult<BackupCaptureReport> {
    if !matches!(operation.state, State::Capturing) {
        return Err(BackupError::Finished);
    }
    operation.state = State::Imaging;
    let result = (|| {
        control.check()?;
        let _memory = control.memory().reserve(size_of::<Machine<'_, S>>())?;
        let mut machine = Machine::new(operation, source, files, expected, control)?;
        while !machine.advance(operation, 128, 16 * 1024)? {}
        machine.report()
    })();
    if result.is_err() {
        operation.state = State::Failed;
    }
    result
}
