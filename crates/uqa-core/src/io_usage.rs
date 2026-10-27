//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual block and journal activity belongs to the executing invocation and its enclosing operations.

use crate::memory::{Budgeted, MemoryBudget, MemoryError};
use parking_lot::Mutex;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;

mod context;
pub use context::{IOContext, IOScope, ObservedBlockOperation};
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockUsage {
    pub hits: u64,
    pub reads: u64,
    pub dirtied: u64,
    pub written: u64,
    pub read_time: Duration,
    pub write_time: Duration,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WALUsage {
    pub records: u64,
    pub full_page_images: u64,
    pub bytes: u64,
    pub buffers_full: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IOUsage {
    pub shared: BlockUsage,
    pub local: BlockUsage,
    pub temporary: BlockUsage,
    pub wal: WALUsage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockClass {
    Shared,
    Local,
    Temporary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockOperation {
    Hit,
    Read,
    Dirty,
    Write,
}

#[derive(Clone, Copy, Debug)]
pub enum IOEvent {
    Blocks {
        class: BlockClass,
        operation: BlockOperation,
        count: u64,
        elapsed: Duration,
    },
    WAL(WALUsage),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct IOOptions {
    pub buffers: bool,
    pub wal: bool,
}

impl IOOptions {
    fn including(self, other: Self) -> Self {
        Self {
            buffers: self.buffers || other.buffers,
            wal: self.wal || other.wal,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("native I/O measurements could not be completed")]
pub struct IOObservationError;

struct Counters {
    usage: IOUsage,
    failed: bool,
    closed: bool,
}

struct Observing {
    parent: Option<IOObservation>,
    options: IOOptions,
    requested: IOOptions,
    timing: Arc<AtomicBool>,
    counters: Mutex<Counters>,
}

/// A bounded owner for one invocation or operator. Workers share this explicit owner; unrelated concurrent invocations have separate roots.
#[derive(Clone)]
pub struct IOObservation(Arc<Budgeted<Observing>>);

impl IOObservation {
    pub fn new(
        budget: &MemoryBudget,
        options: IOOptions,
        timing: Arc<AtomicBool>,
        parent: Option<Self>,
    ) -> Result<Self, MemoryError> {
        let requested = parent
            .as_ref()
            .map_or(options, |parent| options.including(parent.0.requested));
        Ok(Self(
            Budgeted::new(
                Observing {
                    parent,
                    options,
                    requested,
                    timing,
                    counters: Mutex::new(Counters {
                        usage: IOUsage::default(),
                        failed: false,
                        closed: false,
                    }),
                },
                budget.empty_reservation(),
            )
            .into_shared()?,
        ))
    }

    pub fn options(&self) -> IOOptions {
        self.0.options
    }

    pub fn timing_enabled(&self) -> bool {
        self.0.timing.load(std::sync::atomic::Ordering::Acquire)
    }

    /// An operation captures every measurement requested by its invocation or an enclosing invocation.
    pub fn child(&self, budget: &MemoryBudget) -> Result<Self, MemoryError> {
        Self::new(
            budget,
            self.0.requested,
            Arc::clone(&self.0.timing),
            Some(self.clone()),
        )
    }

    pub fn is_within(&self, ancestor: &Self) -> bool {
        let mut selected = Some(self);
        while let Some(observation) = selected {
            if Arc::ptr_eq(&observation.0, &ancestor.0) {
                return true;
            }
            selected = observation.0.parent.as_ref();
        }
        false
    }

    pub fn snapshot(&self) -> Result<IOUsage, IOObservationError> {
        let counters = self.0.counters.lock();
        if counters.failed {
            return Err(IOObservationError);
        }
        Ok(counters.usage)
    }

    /// Close before publishing measurements. A retained worker cannot attribute later work to a completed invocation.
    pub fn finish(&self) -> Result<IOUsage, IOObservationError> {
        let mut counters = self.0.counters.lock();
        counters.closed = true;
        if counters.failed {
            return Err(IOObservationError);
        }
        Ok(counters.usage)
    }

    pub fn record(&self, event: IOEvent) {
        let mut selected = Some(self);
        while let Some(observation) = selected {
            let mut counters = observation.0.counters.lock();
            if counters.closed {
                break;
            }
            let accepted = match event {
                IOEvent::Blocks { .. } => observation.0.options.buffers,
                IOEvent::WAL(_) => observation.0.options.wal,
            };
            if accepted && !counters.failed {
                match counters.usage.add(event) {
                    Some(usage) => counters.usage = usage,
                    None => counters.failed = true,
                }
            }
            drop(counters);
            selected = observation.0.parent.as_ref();
        }
    }
}

impl IOUsage {
    /// Combine disjoint operation totals. Inclusive parent/child observations must not be added together.
    pub fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            shared: self.shared.checked_add(other.shared)?,
            local: self.local.checked_add(other.local)?,
            temporary: self.temporary.checked_add(other.temporary)?,
            wal: WALUsage {
                records: self.wal.records.checked_add(other.wal.records)?,
                full_page_images: self
                    .wal
                    .full_page_images
                    .checked_add(other.wal.full_page_images)?,
                bytes: self.wal.bytes.checked_add(other.wal.bytes)?,
                buffers_full: self.wal.buffers_full.checked_add(other.wal.buffers_full)?,
            },
        })
    }

    fn add(mut self, event: IOEvent) -> Option<Self> {
        match event {
            IOEvent::Blocks {
                class,
                operation,
                count,
                elapsed,
            } => {
                let blocks = match class {
                    BlockClass::Shared => &mut self.shared,
                    BlockClass::Local => &mut self.local,
                    BlockClass::Temporary => &mut self.temporary,
                };
                let destination = match operation {
                    BlockOperation::Hit => &mut blocks.hits,
                    BlockOperation::Read => &mut blocks.reads,
                    BlockOperation::Dirty => &mut blocks.dirtied,
                    BlockOperation::Write => &mut blocks.written,
                };
                *destination = destination.checked_add(count)?;
                match operation {
                    BlockOperation::Read => {
                        blocks.read_time = blocks.read_time.checked_add(elapsed)?;
                    }
                    BlockOperation::Write => {
                        blocks.write_time = blocks.write_time.checked_add(elapsed)?;
                    }
                    BlockOperation::Hit | BlockOperation::Dirty => {}
                }
            }
            IOEvent::WAL(wal) => {
                self.wal.records = self.wal.records.checked_add(wal.records)?;
                self.wal.full_page_images = self
                    .wal
                    .full_page_images
                    .checked_add(wal.full_page_images)?;
                self.wal.bytes = self.wal.bytes.checked_add(wal.bytes)?;
                self.wal.buffers_full = self.wal.buffers_full.checked_add(wal.buffers_full)?;
            }
        }
        Some(self)
    }
}

impl BlockUsage {
    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            hits: self.hits.checked_add(other.hits)?,
            reads: self.reads.checked_add(other.reads)?,
            dirtied: self.dirtied.checked_add(other.dirtied)?,
            written: self.written.checked_add(other.written)?,
            read_time: self.read_time.checked_add(other.read_time)?,
            write_time: self.write_time.checked_add(other.write_time)?,
        })
    }
}
