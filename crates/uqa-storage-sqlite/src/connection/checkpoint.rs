//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! WAL maintenance belongs to the pool, outside foreground transaction publication.

use std::{sync::Arc, thread::JoinHandle, time::Duration};

use parking_lot::{Condvar, Mutex};
use rusqlite::Connection;

use super::{ownership::DatabaseOwner, source::DatabaseSource, ConnectionSpec, Result};

const CHECKPOINT_PAGES: i32 = 1000;
const RETRY: Duration = Duration::from_millis(100);

#[derive(Default)]
struct Request {
    pending: bool,
    stopping: bool,
}

#[derive(Default)]
struct Signal {
    request: Mutex<Request>,
    changed: Condvar,
    #[cfg(test)]
    work_gate: Mutex<()>,
    #[cfg(test)]
    completed: std::sync::atomic::AtomicUsize,
}

pub(super) struct Checkpoint {
    signal: Arc<Signal>,
    worker: Option<JoinHandle<()>>,
}

impl Checkpoint {
    pub(super) fn start(
        spec: &ConnectionSpec,
        source: Option<Arc<DatabaseSource>>,
        owner: Option<Arc<DatabaseOwner>>,
    ) -> Result<Option<Self>> {
        if !matches!(spec, ConnectionSpec::File { .. }) || source.is_none() {
            return Ok(None);
        }
        let signal = Arc::new(Signal::default());
        let worker_signal = Arc::clone(&signal);
        let spec = spec.clone();
        let worker = std::thread::Builder::new()
            .name("uqa-sqlite-checkpoint".into())
            .spawn(move || {
                // The connection closes before ownership is released. Pool shutdown joins this worker before closing its other connections or releasing restoration exclusion.
                let _owner = owner;
                let source = source.expect("file source");
                let mut connection = None;
                let mut retry = false;
                loop {
                    let stopping = {
                        let mut request = worker_signal.request.lock();
                        if retry && !request.stopping {
                            // Coalesce commits and retry readers that temporarily prevent completion without spinning on their retained snapshots.
                            let deadline = std::time::Instant::now() + RETRY;
                            while !request.stopping {
                                let remaining =
                                    deadline.saturating_duration_since(std::time::Instant::now());
                                if remaining.is_zero() {
                                    break;
                                }
                                worker_signal.changed.wait_for(&mut request, remaining);
                            }
                        }
                        while !retry && !request.pending && !request.stopping {
                            worker_signal.changed.wait(&mut request);
                        }
                        request.pending = false;
                        request.stopping
                    };
                    if stopping && connection.is_none() {
                        break;
                    }
                    #[cfg(test)]
                    let _test_gate = worker_signal.work_gate.lock();
                    let result = (|| -> Result<bool> {
                        source.check()?;
                        if connection.is_none() {
                            let opened = spec.open(false)?;
                            source.check_connection(&opened)?;
                            opened.busy_timeout(Duration::ZERO)?;
                            connection = Some(opened);
                        }
                        let connection = connection.as_ref().expect("opened above");
                        let (busy, frames, copied): (i64, i64, i64) =
                            connection.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                            })?;
                        Ok(busy != 0 || copied < frames)
                    })();
                    #[cfg(test)]
                    if matches!(result, Ok(false)) {
                        worker_signal
                            .completed
                            .fetch_add(1, std::sync::atomic::Ordering::Release);
                    }
                    if stopping {
                        break;
                    }
                    // Errors do not alter the already durable foreground commit. Retain the request and retry; source replacement is never adopted.
                    retry = result.unwrap_or(true);
                }
            })?;
        Ok(Some(Self {
            signal,
            worker: Some(worker),
        }))
    }

    /// The pool retains this signal until all hooked physical connections have closed. The hook only schedules work; FULL synchronization of the transaction itself remains on the committing connection.
    #[allow(unsafe_code)]
    pub(super) fn install(&self, connection: &Connection) {
        // SAFETY: every connection installed here belongs to this pool. Pool fields close connections before dropping this signal, and the hook never retains a connection or lets a panic cross the C boundary.
        unsafe {
            rusqlite::ffi::sqlite3_wal_hook(
                connection.handle(),
                Some(schedule),
                Arc::as_ptr(&self.signal).cast_mut().cast(),
            );
        }
    }

    pub(super) fn stop(&mut self) {
        {
            let mut request = self.signal.request.lock();
            request.stopping = true;
            self.signal.changed.notify_one();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Checkpoint {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(unsafe_code)]
unsafe extern "C" fn schedule(
    context: *mut std::ffi::c_void,
    _database: *mut rusqlite::ffi::sqlite3,
    _name: *const std::ffi::c_char,
    pages: std::ffi::c_int,
) -> std::ffi::c_int {
    if pages >= CHECKPOINT_PAGES {
        // SAFETY: install registers a non-null pointer retained by the owning pool through final connection close.
        let signal = unsafe { &*context.cast::<Signal>() };
        signal.request.lock().pending = true;
        signal.changed.notify_one();
    }
    rusqlite::ffi::SQLITE_OK
}

#[cfg(test)]
mod tests;
