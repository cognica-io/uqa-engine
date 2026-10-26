//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session identities isolate nested calls; worker bindings explicitly inherit their captured invocation.

use super::{CapturedDiagnostics, DiagnosticsScope, InvocationDiagnostics};
use std::{cell::RefCell, marker::PhantomData, rc::Rc, sync::Arc};
use uqa_core::io_usage::{IOContext, IOOptions, IOScope};
use uqa_core::memory::Budgeted;
use uqa_storage::{read_control::StorageReadControl, StorageBackendResult};

type Binding = Arc<Budgeted<InvocationBinding>>;

#[derive(Default)]
struct SessionIdentity {
    next_instance: std::sync::atomic::AtomicU64,
    io_timing: Arc<std::sync::atomic::AtomicBool>,
}

thread_local! {
    static CURRENT: RefCell<Option<Binding>> = const { RefCell::new(None) };
    static REQUEST: RefCell<Option<InvocationRequest>> = const { RefCell::new(None) };
}

struct InvocationBinding {
    identity: Arc<SessionIdentity>,
    collector: CapturedDiagnostics,
    previous: Option<Binding>,
}

/// A suspended worker receives the caller's scope on every step, including explicit absence.
#[derive(Clone)]
pub struct InvocationRequest {
    identity: Arc<SessionIdentity>,
    collector: Option<CapturedDiagnostics>,
    control: Option<StorageReadControl>,
    io: IOContext,
}

pub struct RequestScope {
    previous: Option<InvocationRequest>,
    io: IOScope,
    _thread: PhantomData<Rc<()>>,
}

impl RequestScope {
    pub fn replace(&self, request: Option<InvocationRequest>) {
        self.io.replace(
            &request
                .as_ref()
                .map_or_else(IOContext::default, |request| request.io.clone()),
        );
        REQUEST.with(|current| {
            *current.borrow_mut() = request;
        });
    }
}

impl Drop for RequestScope {
    fn drop(&mut self) {
        REQUEST.with(|current| current.borrow_mut().clone_from(&self.previous));
    }
}

#[derive(Default)]
pub struct QueryDiagnostics {
    identity: Arc<SessionIdentity>,
    inherited: Option<CapturedDiagnostics>,
    retained_control: Option<StorageReadControl>,
    inherited_io: IOContext,
}

impl QueryDiagnostics {
    /// All invocation and retained-worker collectors read the setting published by the owning session's state transitions.
    pub fn new(io_timing: Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            identity: Arc::new(SessionIdentity {
                io_timing,
                next_instance: std::sync::atomic::AtomicU64::new(0),
            }),
            inherited: None,
            retained_control: None,
            inherited_io: IOContext::default(),
        }
    }

    pub fn io_timing(&self) -> bool {
        self.identity
            .io_timing
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Select this session's active operation, including an explicit worker request. A different session on the same thread never inherits its I/O counters.
    pub fn io_context(&self) -> IOContext {
        let collector = self.capture();
        let Some(root) = collector
            .as_ref()
            .and_then(|collector| collector.io.as_ref())
        else {
            return IOContext::default();
        };
        let current = IOContext::current();
        if current
            .observation()
            .is_some_and(|current| current.is_within(root))
        {
            return current;
        }
        if let Some(request) = REQUEST
            .with(|request| {
                request
                    .borrow()
                    .as_ref()
                    .filter(|request| Arc::ptr_eq(&request.identity, &self.identity))
                    .map(|request| request.io.clone())
            })
            .filter(|context| context.observation().is_some_and(|io| io.is_within(root)))
        {
            return request;
        }
        if self
            .inherited_io
            .observation()
            .is_some_and(|io| io.is_within(root))
        {
            return self.inherited_io.clone();
        }
        IOContext::new(Some(root.clone()))
    }

    pub fn capture(&self) -> Option<CapturedDiagnostics> {
        CURRENT.with(|current| {
            let current = current.borrow();
            let mut binding = current.as_ref();
            while let Some(selected) = binding {
                if Arc::ptr_eq(&selected.identity, &self.identity) {
                    return selected
                        .collector
                        .is_active()
                        .then(|| Arc::clone(&selected.collector));
                }
                binding = selected.previous.as_ref();
            }
            REQUEST.with(|request| {
                if let Some(request) = request
                    .borrow()
                    .as_ref()
                    .filter(|request| Arc::ptr_eq(&request.identity, &self.identity))
                {
                    return request
                        .collector
                        .clone()
                        .filter(|collector| collector.is_active());
                }
                self.inherited
                    .clone()
                    .filter(|collector| collector.is_active())
            })
        })
    }

    /// Bind inherited worker state before executing expressions or host callbacks.
    pub fn bind_current(&self) -> StorageBackendResult<Option<CapturedScope>> {
        // A streaming query can span several FETCH invocations. Its request binding remains live until the next request; pinning it here would attribute later work to an earlier caller.
        if REQUEST.with(|request| {
            request
                .borrow()
                .as_ref()
                .is_some_and(|request| Arc::ptr_eq(&request.identity, &self.identity))
        }) {
            return Ok(None);
        }
        self.capture()
            .as_ref()
            .map(|collector| self.bind(collector))
            .transpose()
    }

    pub fn request(&self) -> InvocationRequest {
        let collector = self.capture();
        let control = collector
            .as_ref()
            .map(|collector| collector.control().clone())
            .or_else(|| self.retained_control.clone());
        InvocationRequest {
            identity: Arc::clone(&self.identity),
            collector,
            control,
            io: self.io_context(),
        }
    }

    /// Retained physical inputs keep the invoking read allowance even before their first measured FETCH.
    pub fn request_with_control(&self, control: StorageReadControl) -> InvocationRequest {
        InvocationRequest {
            identity: Arc::clone(&self.identity),
            collector: self.capture(),
            control: Some(control),
            io: self.io_context(),
        }
    }

    pub fn request_scope(request: Option<InvocationRequest>) -> RequestScope {
        let io = request
            .as_ref()
            .map_or_else(IOContext::default, |request| request.io.clone())
            .enter();
        let previous = REQUEST.with(|current| current.replace(request));
        RequestScope {
            previous,
            io,
            _thread: PhantomData,
        }
    }

    /// Keep session identity while giving the existing read worker an immutable inherited scope.
    pub fn fork(&self) -> Self {
        Self {
            identity: Arc::clone(&self.identity),
            inherited: self.capture(),
            retained_control: self.retained_control.clone(),
            inherited_io: self.io_context(),
        }
    }

    /// Capture a retained child worker's original read owner along with its session identity.
    pub fn fork_with_control(&self, control: StorageReadControl) -> Self {
        Self {
            identity: Arc::clone(&self.identity),
            inherited: self.capture(),
            retained_control: Some(control),
            inherited_io: self.io_context(),
        }
    }

    pub fn enter(&self, control: StorageReadControl) -> StorageBackendResult<DiagnosticsScope> {
        self.enter_selected(control, None, IOOptions::default())
    }

    pub fn enter_measured(
        &self,
        control: StorageReadControl,
        timing: bool,
        io: IOOptions,
    ) -> StorageBackendResult<DiagnosticsScope> {
        self.enter_selected(control, Some(timing), io)
    }

    pub(super) fn request_control(&self) -> Option<StorageReadControl> {
        REQUEST.with(|request| {
            request
                .borrow()
                .as_ref()
                .filter(|request| Arc::ptr_eq(&request.identity, &self.identity))
                .and_then(|request| request.control.clone())
        })
    }

    pub(super) fn allocate_instance(&self) -> Result<u64, uqa_sql::SQLError> {
        use std::sync::atomic::Ordering::Relaxed;
        self.identity
            .next_instance
            .fetch_update(Relaxed, Relaxed, |next| next.checked_add(1))
            .map_err(|_| {
                uqa_sql::SQLError::Internal("native execution instance identities exhausted".into())
            })
    }

    fn enter_selected(
        &self,
        control: StorageReadControl,
        timing: Option<bool>,
        io: IOOptions,
    ) -> StorageBackendResult<DiagnosticsScope> {
        let collector = InvocationDiagnostics::new(
            control,
            self.capture(),
            timing,
            io,
            Arc::clone(&self.identity.io_timing),
        )?;
        let binding = self.bind(&collector)?;
        Ok(DiagnosticsScope {
            _binding: binding,
            collector,
        })
    }

    /// Install the driver's fixed capture on the executing thread before invoking any callbacks.
    pub fn bind(&self, collector: &CapturedDiagnostics) -> StorageBackendResult<CapturedScope> {
        collector.control().check()?;
        CURRENT.with(|current| {
            let value = InvocationBinding {
                identity: Arc::clone(&self.identity),
                collector: Arc::clone(collector),
                previous: current.borrow().clone(),
            };
            let binding = Budgeted::new(value, collector.control().memory().empty_reservation())
                .into_shared()?;
            *current.borrow_mut() = Some(Arc::clone(&binding));
            Ok(CapturedScope {
                binding,
                _io: self.io_context().enter(),
                _thread: PhantomData,
            })
        })
    }
}

/// A dynamic binding must be restored on the thread that entered it.
pub struct CapturedScope {
    binding: Binding,
    _io: IOScope,
    _thread: PhantomData<Rc<()>>,
}

impl Drop for CapturedScope {
    fn drop(&mut self) {
        CURRENT.with(|current| {
            let mut current = current.borrow_mut();
            debug_assert!(current
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, &self.binding)));
            current.clone_from(&self.binding.previous);
        });
    }
}
