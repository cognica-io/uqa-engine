//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session identities isolate nested calls; worker bindings explicitly inherit their captured invocation.

use super::{CapturedDiagnostics, DiagnosticsScope, InvocationDiagnostics};
use std::{cell::RefCell, marker::PhantomData, rc::Rc, sync::Arc};
use uqa_core::memory::Budgeted;
use uqa_storage::{read_control::StorageReadControl, StorageBackendResult};

type Binding = Arc<Budgeted<InvocationBinding>>;

#[derive(Default)]
struct SessionIdentity {
    next_instance: std::sync::atomic::AtomicU64,
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
}

pub struct RequestScope {
    previous: Option<InvocationRequest>,
    _thread: PhantomData<Rc<()>>,
}

impl RequestScope {
    pub fn replace(&self, request: Option<InvocationRequest>) {
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
}

impl QueryDiagnostics {
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
        }
    }

    /// Retained physical inputs keep the invoking read allowance even before their first measured FETCH.
    pub fn request_with_control(&self, control: StorageReadControl) -> InvocationRequest {
        InvocationRequest {
            identity: Arc::clone(&self.identity),
            collector: self.capture(),
            control: Some(control),
        }
    }

    pub fn request_scope(request: Option<InvocationRequest>) -> RequestScope {
        let previous = REQUEST.with(|current| current.replace(request));
        RequestScope {
            previous,
            _thread: PhantomData,
        }
    }

    /// Keep session identity while giving the existing read worker an immutable inherited scope.
    pub fn fork(&self) -> Self {
        Self {
            identity: Arc::clone(&self.identity),
            inherited: self.capture(),
            retained_control: self.retained_control.clone(),
        }
    }

    /// Capture a retained child worker's original read owner along with its session identity.
    pub fn fork_with_control(&self, control: StorageReadControl) -> Self {
        Self {
            identity: Arc::clone(&self.identity),
            inherited: self.capture(),
            retained_control: Some(control),
        }
    }

    pub fn enter(&self, control: StorageReadControl) -> StorageBackendResult<DiagnosticsScope> {
        self.enter_selected(control, None)
    }

    pub fn enter_measured(
        &self,
        control: StorageReadControl,
        timing: bool,
    ) -> StorageBackendResult<DiagnosticsScope> {
        self.enter_selected(control, Some(timing))
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
    ) -> StorageBackendResult<DiagnosticsScope> {
        let collector = InvocationDiagnostics::new(control, self.capture(), timing)?;
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
                _thread: PhantomData,
            })
        })
    }
}

/// A dynamic binding must be restored on the thread that entered it.
pub struct CapturedScope {
    binding: Binding,
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
