//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invocation scopes and bounded records for actual native execution.

use parking_lot::Mutex;
use std::sync::Arc;
use uqa_core::{
    io_usage::{IOContext, IOObservation, IOOptions, IOUsage},
    memory::{Budgeted, BudgetedString, BudgetedVec, MemoryError, MemoryReservation},
    vector_execution::VectorSearchOperation,
    VectorGeneration,
};
use uqa_operators::vector::diagnostics::VectorSearchObserver;
use uqa_sql::{
    result::vector::{ExplainVectorSearch, ExplainVectorSearches},
    SQLError,
};
use uqa_storage::{
    read_control::StorageReadControl, vector_index::VectorQueryResult, StorageBackendError,
    StorageBackendResult,
};

pub(crate) mod executions;
mod scope;
pub use scope::{CapturedScope, InvocationRequest, QueryDiagnostics, RequestScope};

pub type CapturedDiagnostics = Arc<Budgeted<InvocationDiagnostics>>;

/// Existing vector observers use the same invocation owner.
pub type VectorDiagnostics = InvocationDiagnostics;

struct Records {
    searches: ExplainVectorSearches,
    failed: Option<Failure>,
    closed: bool,
    executions: Option<executions::Executions>,
}

#[derive(Clone, Copy)]
enum Failure {
    Memory,
    Cancelled,
    Closed,
    ExecutionMemory,
    Measurement,
}

impl Failure {
    fn error(self) -> SQLError {
        match self {
            Self::Cancelled => uqa_core::QueryCancelled::USER_REQUEST.into(),
            Self::Memory => SQLError::Routine {
                sqlstate: "53200".into(),
                message: "EXPLAIN could not retain complete vector execution diagnostics".into(),
            },
            Self::Closed => SQLError::Internal("vector diagnostics scope is closed".into()),
            Self::ExecutionMemory => SQLError::Routine {
                sqlstate: "53200".into(),
                message: "EXPLAIN could not retain complete native execution diagnostics".into(),
            },
            Self::Measurement => SQLError::Internal(
                "EXPLAIN native execution measurements could not be completed".into(),
            ),
        }
    }
}

/// A captured invocation is shared by parallel work; it never samples a mutable current scope.
pub struct InvocationDiagnostics {
    control: StorageReadControl,
    parent: Option<CapturedDiagnostics>,
    io: Option<IOObservation>,
    records: Mutex<Records>,
}

impl InvocationDiagnostics {
    fn new(
        control: StorageReadControl,
        parent: Option<CapturedDiagnostics>,
        timing: Option<bool>,
        io_options: IOOptions,
        io_timing: Arc<std::sync::atomic::AtomicBool>,
    ) -> StorageBackendResult<CapturedDiagnostics> {
        control.check()?;
        let parent_io =
            IOContext::for_observation(parent.as_ref().and_then(|parent| parent.io.as_ref()));
        let io = if io_options.buffers || io_options.wal || parent_io.observation().is_some() {
            Some(IOObservation::new(
                control.memory(),
                io_options,
                io_timing,
                parent_io.observation().cloned(),
            )?)
        } else {
            None
        };
        let records = Mutex::new(Records {
            searches: BudgetedVec::new(control.memory()),
            failed: None,
            closed: false,
            executions: timing.map(|timing| executions::Executions::new(&control, timing)),
        });
        let memory = control.memory().empty_reservation();
        Ok(Budgeted::new(
            Self {
                control,
                parent,
                io,
                records,
            },
            memory,
        )
        .into_shared()?)
    }

    fn is_active(&self) -> bool {
        !self.records.lock().closed
    }

    pub fn control(&self) -> &StorageReadControl {
        &self.control
    }

    /// Bind only contexts that are about to execute inside this invocation.
    pub fn observer(
        collector: &CapturedDiagnostics,
        relation: &str,
    ) -> StorageBackendResult<Arc<dyn VectorSearchObserver>> {
        let mut name = BudgetedString::new(collector.control.memory());
        name.push_str(relation)?;
        let value = BoundObserver {
            collector: Arc::clone(collector),
            relation: name,
            _memory: collector
                .control
                .memory()
                .reserve(std::mem::size_of::<BoundObserver>())?,
        };
        Ok(Arc::new(value))
    }

    pub fn record(
        &self,
        relation: &str,
        field: &str,
        operation: VectorSearchOperation,
        result: &VectorQueryResult,
    ) -> StorageBackendResult<()> {
        let Some(stats) = result.diskann else {
            return Ok(());
        };
        let recorded = (|| {
            self.control.check()?;
            let mut relation_name = BudgetedString::new(self.control.memory());
            relation_name.push_str(relation)?;
            let mut field_name = BudgetedString::new(self.control.memory());
            field_name.push_str(field)?;
            let generation = stats.generation;
            let record = ExplainVectorSearch {
                relation: relation_name,
                field: field_name,
                operation,
                returned_documents: u64::try_from(result.postings.len())
                    .map_err(|_| MemoryError::SizeOverflow)?,
                generation: VectorGeneration {
                    database: generation.database(),
                    table: generation.table(),
                    index: generation.index(),
                    generation: generation.generation(),
                },
                route: stats.route,
                traversal: stats.traversal,
                work: stats.work,
            };
            let record =
                Budgeted::new(record, self.control.memory().empty_reservation()).into_shared()?;
            let mut current = Some(self);
            while let Some(collector) = current {
                collector.control.check()?;
                let mut records = collector.records.lock();
                if records.closed {
                    return Err(StorageBackendError::Other(
                        "vector diagnostics scope is closed".into(),
                    ));
                }
                records.searches.push(Arc::clone(&record))?;
                current = collector.parent.as_ref().map(|parent| &***parent);
            }
            Ok(())
        })();
        if let Err(error) = &recorded {
            let failure = match error {
                StorageBackendError::Cancelled(_) => Failure::Cancelled,
                StorageBackendError::Memory(_) => Failure::Memory,
                _ => Failure::Closed,
            };
            let mut current = Some(self);
            while let Some(collector) = current {
                let mut records = collector.records.lock();
                if records.closed {
                    break;
                }
                records.failed.get_or_insert(failure);
                current = collector.parent.as_ref().map(|parent| &***parent);
            }
        }
        recorded
    }

    fn finish(&self) -> Result<CapturedExecution, SQLError> {
        let records = self.close();
        self.control.cancellation().check()?;
        if let Some(failure) = records.failed {
            return Err(failure.error());
        }
        let nodes = records.executions.as_ref().map_or_else(
            || Ok(BudgetedVec::new(self.control.memory())),
            |nodes| nodes.snapshot(&self.control),
        )?;
        Ok(CapturedExecution {
            io_timing: self.io.as_ref().is_some_and(IOObservation::timing_enabled),
            io: self
                .io
                .as_ref()
                .map_or(Ok(IOUsage::default()), IOObservation::finish)
                .map_err(|_| Failure::Measurement.error())?,
            vector_searches: records.searches,
            nodes,
        })
    }

    /// Closing transfers all retained records out of the collector. Worker captures keep only a closed owner, on success, error and unwind alike.
    fn close(&self) -> Records {
        let records = std::mem::replace(
            &mut *self.records.lock(),
            Records {
                searches: BudgetedVec::new(self.control.memory()),
                failed: None,
                closed: true,
                executions: None,
            },
        );
        if let Some(io) = &self.io {
            let _ = io.finish();
        }
        records
    }
}

struct BoundObserver {
    collector: CapturedDiagnostics,
    relation: BudgetedString,
    _memory: MemoryReservation,
}

impl VectorSearchObserver for BoundObserver {
    fn control(&self) -> &StorageReadControl {
        self.collector.control()
    }
    fn record(
        &self,
        field: &str,
        operation: VectorSearchOperation,
        result: &VectorQueryResult,
    ) -> StorageBackendResult<()> {
        self.collector
            .record(&self.relation, field, operation, result)
    }
}

pub struct CapturedExecution {
    pub io: IOUsage,
    pub io_timing: bool,
    pub vector_searches: ExplainVectorSearches,
    pub nodes: BudgetedVec<uqa_sql::result::explain::ExplainNodeExecution>,
}

/// The guard restores its caller's binding on success, SQL error and unwind.
pub struct DiagnosticsScope {
    _binding: CapturedScope,
    collector: CapturedDiagnostics,
}

impl DiagnosticsScope {
    pub fn finish_execution(self) -> Result<CapturedExecution, SQLError> {
        self.collector.finish()
    }

    pub fn finish(self) -> Result<ExplainVectorSearches, SQLError> {
        Ok(self.collector.finish()?.vector_searches)
    }
}

impl Drop for DiagnosticsScope {
    fn drop(&mut self) {
        drop(self.collector.close());
    }
}

#[cfg(test)]
mod io_tests;
#[cfg(test)]
mod tests;
