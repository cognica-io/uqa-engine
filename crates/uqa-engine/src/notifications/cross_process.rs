//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-process listener leases, committed-queue reads, registry transactions, and wakeups.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use super::NotificationHub;
use fs2::FileExt;
use parking_lot::Mutex;
use uqa_sql::SQLError;
use uqa_storage::StorageEncryptionKey;
use uqa_storage::{
    notifications::{NotificationPublication, PendingNotification},
    read_control::StorageReadControl,
    PersistentStorageBackend,
};
use uqa_storage_sqlite::notifications::NotificationRegistry;

pub(super) use uqa_storage::notifications::{
    NotificationListenerRow as CrossProcessListenerRow,
    NotificationQueueEntry as CrossProcessQueueEntry,
    NotificationQueueState as CrossProcessQueueState,
};

pub(super) struct ListenerLease {
    owner_id: [u8; 16],
    path: PathBuf,
    file: Option<File>,
}

impl ListenerLease {
    pub(super) const fn owner_id(&self) -> [u8; 16] {
        self.owner_id
    }
}

impl Drop for ListenerLease {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = FileExt::unlock(&file);
            drop(file);
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(super) struct CrossProcessRegistryTransaction {
    transaction: uqa_storage_sqlite::notifications::NotificationRegistryTransaction,
}

impl CrossProcessRegistryTransaction {
    pub(super) const fn pending_acknowledgement(&self) -> Option<[u8; 32]> {
        self.transaction.pending_acknowledgement()
    }
    pub(super) fn prepare_publication(
        &mut self,
        process_id: i32,
        pending: &[PendingNotification],
        listener: Option<&CrossProcessListenerRow>,
        control: &StorageReadControl,
    ) -> Result<NotificationPublication, SQLError> {
        self.transaction
            .prepare_publication(process_id, pending, listener, control)
            .map_err(registry_error)
    }

    pub(super) fn allocate_backend_process_id(&self) -> Result<i32, SQLError> {
        self.transaction
            .allocate_backend_process_id()
            .map_err(registry_error)
    }

    pub(super) fn queue_state(&self) -> Result<CrossProcessQueueState, SQLError> {
        self.transaction.queue_state().map_err(registry_error)
    }

    pub(super) fn entries_from(
        &self,
        from_sequence: u64,
    ) -> Result<Vec<CrossProcessQueueEntry>, SQLError> {
        self.transaction
            .entries_from(from_sequence)
            .map_err(registry_error)
    }

    pub(super) fn delete_entries_before(&self, sequence: u64) -> Result<(), SQLError> {
        self.transaction
            .delete_entries_before(sequence)
            .map_err(registry_error)
    }

    pub(super) fn listeners(&self) -> Result<Vec<CrossProcessListenerRow>, SQLError> {
        self.transaction.listeners().map_err(registry_error)
    }

    pub(super) fn save_listener(&self, listener: &CrossProcessListenerRow) -> Result<(), SQLError> {
        self.transaction
            .save_listener(listener)
            .map_err(registry_error)
    }

    pub(super) fn drop_listener(
        &self,
        owner_id: [u8; 16],
        session_id: u64,
    ) -> Result<(), SQLError> {
        self.transaction
            .drop_listener(owner_id, session_id)
            .map_err(registry_error)
    }

    pub(super) fn commit(self) -> Result<(), SQLError> {
        self.transaction.commit().map_err(registry_error)
    }
}

fn registry_error(error: uqa_storage::StorageBackendError) -> SQLError {
    match error {
        uqa_storage::StorageBackendError::Other(message) => SQLError::Internal(message),
        error => uqa_execution::storage_errors::storage_error(
            "asynchronous notification registry",
            &error,
        ),
    }
}

fn open_registry_transaction(
    registry: &NotificationRegistry,
) -> Result<CrossProcessRegistryTransaction, SQLError> {
    registry
        .begin()
        .map(|transaction| CrossProcessRegistryTransaction { transaction })
        .map_err(registry_error)
}

pub(super) fn open_registry(
    database_path: &Path,
    key: Option<&StorageEncryptionKey>,
) -> Result<NotificationRegistry, String> {
    NotificationRegistry::open(database_path, key).map_err(|error| error.to_string())
}

pub(super) struct CrossProcessCoordinator {
    database_path: PathBuf,
    registry: NotificationRegistry,
    wake_port: u16,
    shutdown: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
    recovery: Mutex<Option<RecoverySession>>,
}

#[derive(Clone)]
struct RecoverySession {
    backend: Arc<dyn PersistentStorageBackend>,
    control: StorageReadControl,
}

impl CrossProcessCoordinator {
    pub(super) fn allocate_backend_process_id(
        registry: &NotificationRegistry,
    ) -> Result<i32, SQLError> {
        let transaction = open_registry_transaction(registry)?;
        let process_id = transaction.allocate_backend_process_id()?;
        transaction.commit()?;
        Ok(process_id)
    }

    pub(super) fn open(
        database_path: &Path,
        registry: NotificationRegistry,
    ) -> Result<(Self, TcpListener), String> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .map_err(|error| format!("bind asynchronous notification wake listener: {error}"))?;
        let wake_port = listener
            .local_addr()
            .map_err(|error| format!("read asynchronous notification wake address: {error}"))?
            .port();
        Ok((
            Self {
                database_path: database_path.to_path_buf(),
                registry,
                wake_port,
                shutdown: Arc::new(AtomicBool::new(false)),
                worker: Mutex::new(None),
                recovery: Mutex::new(None),
            },
            listener,
        ))
    }

    pub(super) fn start_worker(
        &self,
        listener: TcpListener,
        hub: Weak<NotificationHub>,
    ) -> Result<(), String> {
        listener.set_nonblocking(true).map_err(|error| {
            format!("configure asynchronous notification recovery polling: {error}")
        })?;
        let shutdown = Arc::clone(&self.shutdown);
        let worker = std::thread::Builder::new()
            .name("uqa-notification-wake".into())
            .spawn(move || loop {
                match listener.accept() {
                    Ok((stream, _)) => drop(stream),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::park_timeout(Duration::from_millis(100));
                    }
                    Err(error) => {
                        if let Some(hub) = hub.upgrade() {
                            hub.record_cross_error(format!(
                                "accept asynchronous notification wake connection: {error}"
                            ));
                        }
                        break;
                    }
                }
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                let Some(hub) = hub.upgrade() else { break };
                hub.synchronize_cross_process_notifications();
            })
            .map_err(|error| format!("spawn asynchronous notification wake worker: {error}"))?;
        *self.worker.lock() = Some(worker);
        Ok(())
    }

    pub(super) const fn wake_port(&self) -> u16 {
        self.wake_port
    }

    pub(super) fn begin_registry_transaction(
        &self,
    ) -> Result<CrossProcessRegistryTransaction, SQLError> {
        let recovery = self.recovery.lock().clone().ok_or_else(|| {
            SQLError::Internal("notification recovery has no independent storage session".into())
        })?;
        let store = recovery
            .backend
            .notification_publications()
            .ok_or_else(|| {
                SQLError::Internal(
                    "notification recovery session omitted atomic publication".into(),
                )
            })?;
        self.registry
            .begin_recovered(store, &recovery.control)
            .map(|transaction| CrossProcessRegistryTransaction { transaction })
            .map_err(registry_error)
    }

    pub(super) fn initialize_recovery(
        &self,
        backend: &Arc<dyn PersistentStorageBackend>,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        let mut recovery = self.recovery.lock();
        if recovery.is_none() {
            let session = backend.open_session().map_err(registry_error)?;
            if session.backend.notification_publications().is_none() {
                return Err(SQLError::Internal(
                    "persistent backend does not support atomic notification publication".into(),
                ));
            }
            let allowance = session
                .backend
                .retention_control()
                .unwrap_or_else(|| control.clone());
            *recovery = Some(RecoverySession {
                backend: session.backend,
                control: StorageReadControl::new(
                    allowance.memory(),
                    &uqa_core::CancellationToken::new(),
                ),
            });
        }
        Ok(())
    }

    pub(super) fn recovery_control(&self) -> Result<StorageReadControl, SQLError> {
        self.recovery
            .lock()
            .as_ref()
            .map(|session| session.control.clone())
            .ok_or_else(|| {
                SQLError::Internal(
                    "notification recovery session omitted its retention allowance".into(),
                )
            })
    }

    pub(super) fn recovery_initialized(&self) -> bool {
        self.recovery.lock().is_some()
    }

    pub(super) fn poll_needed(&self, local_owners: &[[u8; 16]]) -> Result<bool, SQLError> {
        let Some(recovery) = self.recovery.lock().clone() else {
            return Ok(false);
        };
        let store = recovery
            .backend
            .notification_publications()
            .ok_or_else(|| {
                SQLError::Internal(
                    "notification recovery session omitted atomic publication".into(),
                )
            })?;
        self.registry
            .poll_needed(store, local_owners, &recovery.control)
            .map_err(registry_error)
    }

    pub(super) fn create_listener_lease(&self) -> Result<ListenerLease, SQLError> {
        for _ in 0..16 {
            let mut owner_id = [0_u8; 16];
            getrandom::fill(&mut owner_id).map_err(|error| {
                SQLError::Internal(format!(
                    "allocate asynchronous notification listener identity: {error}"
                ))
            })?;
            let path = lease_path(&self.database_path, owner_id);
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    file.lock_exclusive().map_err(|error| {
                        SQLError::Internal(format!(
                            "lock asynchronous notification listener lease `{}`: {error}",
                            path.display()
                        ))
                    })?;
                    return Ok(ListenerLease {
                        owner_id,
                        path,
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(SQLError::Internal(format!(
                        "create asynchronous notification listener lease `{}`: {error}",
                        path.display()
                    )));
                }
            }
        }
        Err(SQLError::Internal(
            "could not allocate a unique asynchronous notification listener lease".into(),
        ))
    }

    pub(super) fn listener_is_alive(
        &self,
        owner_id: [u8; 16],
        local_owner_ids: &[[u8; 16]],
    ) -> Result<bool, SQLError> {
        if local_owner_ids.contains(&owner_id) {
            return Ok(true);
        }
        let path = lease_path(&self.database_path, owner_id);
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(SQLError::Internal(format!(
                    "open asynchronous notification listener lease `{}`: {error}",
                    path.display()
                )));
            }
        };
        match file.try_lock_exclusive() {
            Ok(()) => {
                FileExt::unlock(&file).map_err(|error| {
                    SQLError::Internal(format!(
                        "unlock stale asynchronous notification listener lease `{}`: {error}",
                        path.display()
                    ))
                })?;
                drop(file);
                let _ = std::fs::remove_file(path);
                Ok(false)
            }
            Err(error) if error.kind() == fs2::lock_contended_error().kind() => Ok(true),
            Err(error) => Err(SQLError::Internal(format!(
                "probe asynchronous notification listener lease `{}`: {error}",
                path.display()
            ))),
        }
    }

    pub(super) fn wake(ports: &[u16]) {
        for port in ports {
            let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, *port));
            if let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(100))
            {
                let _ = stream.write_all(&[1]);
            }
        }
    }
}

impl Drop for CrossProcessCoordinator {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, self.wake_port));
        let _ = TcpStream::connect_timeout(&address, Duration::from_millis(100));
        if let Some(worker) = self.worker.lock().take() {
            worker.thread().unpark();
            if worker.thread().id() != std::thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}

fn lease_path(database_path: &Path, owner_id: [u8; 16]) -> PathBuf {
    let mut suffix = String::with_capacity(2 * owner_id.len() + 24);
    suffix.push_str(".uqa-notification-");
    for byte in owner_id {
        use std::fmt::Write as _;
        write!(suffix, "{byte:02x}").expect("write listener lease suffix");
    }
    suffix.push_str(".lease");
    suffixed_path(database_path, &suffix)
}

fn suffixed_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut path = database_path.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}
