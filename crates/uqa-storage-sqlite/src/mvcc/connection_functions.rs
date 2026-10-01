//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL functions that record guards and native change capture call, registered once for each connection.
//!
//! `sqlite3CreateFunc` expires every prepared statement of a connection when it replaces an existing function, so the next step of each cached statement recompiles it. Write admission and change capture therefore change only the state these functions share, and the functions stay registered for the connection's lifetime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Weak};

use parking_lot::Mutex;
use rusqlite::{functions::FunctionFlags, Connection};
use uqa_storage::{mvcc::VersionError, read_control::StorageReadControl};

use super::PhysicalResult;

const TOKEN_FUNCTION: &str = "__uqa_mvcc_connection_token";

/// The state of each registered connection, keyed by the token its own token function returns.
static CONNECTIONS: LazyLock<Mutex<HashMap<u64, Weak<ConnectionFunctions>>>> =
    LazyLock::new(Mutex::default);
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// The state that one connection's guard and capture functions share.
pub(in crate::mvcc) struct ConnectionFunctions {
    write_permit: AtomicBool,
    capture: Mutex<Option<CaptureScope>>,
}

/// One materialization's capture: its read control and the first encoding error, which the materialization reports instead of `SQLite`'s generic function failure.
struct CaptureScope {
    control: StorageReadControl,
    error: Option<VersionError>,
}

/// Removes a connection's entry when `SQLite` destroys its token function as the connection closes.
struct Registration(u64);

impl Drop for Registration {
    fn drop(&mut self) {
        CONNECTIONS.lock().remove(&self.0);
    }
}

impl ConnectionFunctions {
    /// The functions of `connection`, registered on its first use. Registration precedes every statement that calls them, so it replaces no function and expires no prepared statement.
    pub(in crate::mvcc) fn of(connection: &Connection) -> PhysicalResult<Arc<Self>> {
        match connection.prepare_cached(&format!("SELECT {TOKEN_FUNCTION}()")) {
            Ok(mut statement) => {
                let token = statement.query_row([], |row| row.get::<_, i64>(0))?;
                u64::try_from(token)
                    .ok()
                    .and_then(|token| CONNECTIONS.lock().get(&token).and_then(Weak::upgrade))
                    .ok_or_else(|| {
                        VersionError::InvalidEncoding("connection function state is unavailable")
                            .into()
                    })
            }
            Err(error) => {
                // Only a connection that never registered its functions lacks the token function; any other preparation failure is reported.
                let registered: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_function_list WHERE name = ?1)",
                    [TOKEN_FUNCTION],
                    |row| row.get(0),
                )?;
                if registered {
                    return Err(error.into());
                }
                Self::register(connection)
            }
        }
    }

    /// Record writes require full synchronization from the first admission onward. The token function is registered last, so a failed registration is retried in full on the next use.
    fn register(connection: &Connection) -> PhysicalResult<Arc<Self>> {
        connection.pragma_update(None, "synchronous", "FULL")?;
        let functions = Arc::new(Self {
            write_permit: AtomicBool::new(false),
            capture: Mutex::new(None),
        });
        let permit = Arc::clone(&functions);
        connection.create_scalar_function(
            "__uqa_mvcc_write_permit",
            0,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
            move |_| Ok(i64::from(permit.write_permit.load(Ordering::Acquire))),
        )?;
        let capture = Arc::clone(&functions);
        connection.create_scalar_function(
            "__uqa_mvcc_native_key",
            -1,
            FunctionFlags::SQLITE_UTF8
                | FunctionFlags::SQLITE_INNOCUOUS
                | FunctionFlags::SQLITE_DETERMINISTIC,
            move |context| capture.capture_key(context),
        )?;
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        CONNECTIONS.lock().insert(token, Arc::downgrade(&functions));
        let registration = Registration(token);
        let value = i64::try_from(token).map_err(|_| {
            VersionError::InvalidEncoding("connection function tokens are exhausted")
        })?;
        connection.create_scalar_function(
            TOKEN_FUNCTION,
            0,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
            move |_| {
                let Registration(_) = &registration;
                Ok(value)
            },
        )?;
        Ok(functions)
    }

    pub(in crate::mvcc) fn open_write_permit(&self) {
        self.write_permit.store(true, Ordering::Release);
    }

    pub(in crate::mvcc) fn close_write_permit(&self) {
        self.write_permit.store(false, Ordering::Release);
    }

    /// Capture native keys under `control` until [`Self::end_capture`]. Materializations do not nest on one connection.
    pub(in crate::mvcc) fn begin_capture(
        &self,
        control: &StorageReadControl,
    ) -> PhysicalResult<()> {
        let mut capture = self.capture.lock();
        if capture.is_some() {
            return Err(VersionError::InvalidEncoding(
                "native change capture is already active on this connection",
            )
            .into());
        }
        *capture = Some(CaptureScope {
            control: control.clone(),
            error: None,
        });
        Ok(())
    }

    /// End the active capture and return its first encoding error.
    pub(in crate::mvcc) fn end_capture(&self) -> Option<VersionError> {
        self.capture.lock().take().and_then(|scope| scope.error)
    }

    /// Take the active capture's first encoding error, leaving the capture active.
    pub(in crate::mvcc) fn take_capture_error(&self) -> Option<VersionError> {
        self.capture
            .lock()
            .as_mut()
            .and_then(|scope| scope.error.take())
    }

    /// A native change outside a materialization is rejected, as on a connection that never materialized.
    fn capture_key(
        &self,
        context: &rusqlite::functions::Context<'_>,
    ) -> rusqlite::Result<super::native::CapturedKey> {
        let mut capture = self.capture.lock();
        let Some(scope) = capture.as_mut() else {
            return Err(rusqlite::Error::UserFunctionError(Box::new(
                std::io::Error::other("native key capture requires an active materialization"),
            )));
        };
        super::native::capture_key(context, &scope.control).map_err(|error| {
            scope.error.get_or_insert(error);
            rusqlite::Error::UserFunctionError(Box::new(std::io::Error::other(
                "native key capture failed",
            )))
        })
    }
}

#[cfg(test)]
mod tests;
