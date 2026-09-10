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
use rusqlite::{params, Connection, TransactionBehavior};
use uqa_sql::SQLError;

const REGISTRY_SCHEMA_VERSION: i64 = 1;
const REGISTRY_APPLICATION_ID: i64 = 0x5551_4e31;
const REGISTRY_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct CrossProcessQueueState {
    pub(super) next_sequence: u64,
    pub(super) head_position: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CrossProcessQueueEntry {
    pub(super) sequence: u64,
    pub(super) process_id: i32,
    pub(super) channel: String,
    pub(super) payload: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CrossProcessListenerRow {
    pub(super) owner_id: [u8; 16],
    pub(super) session_id: u64,
    pub(super) process_id: i32,
    pub(super) wake_port: u16,
    pub(super) channels: Vec<String>,
    pub(super) transaction_open: bool,
    pub(super) next_sequence: u64,
    pub(super) position: u64,
}

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
    connection: Connection,
    finished: bool,
}

impl CrossProcessRegistryTransaction {
    pub(super) fn allocate_backend_process_id(&self) -> Result<i32, SQLError> {
        let next = self
            .connection
            .query_row(
                "SELECT next_process_id FROM backend_process_id_state WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| registry_error("load backend process identifier", &error))?;
        let process_id = i32::try_from(next).map_err(|_| {
            SQLError::Internal(
                "exhausted positive cross-process backend process identifiers".into(),
            )
        })?;
        if process_id <= 0 {
            return Err(SQLError::Internal(format!(
                "corrupt asynchronous notification backend process identifier {process_id}"
            )));
        }
        self.connection
            .execute(
                "UPDATE backend_process_id_state SET next_process_id = ?1 WHERE singleton = 1",
                params![next + 1],
            )
            .map_err(|error| registry_error("advance backend process identifier", &error))?;
        Ok(process_id)
    }

    pub(super) fn queue_state(&self) -> Result<CrossProcessQueueState, SQLError> {
        self.connection
            .query_row(
                "SELECT next_sequence, head_position FROM queue_state WHERE singleton = 1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(|error| registry_error("load queue state", &error))
            .and_then(|(next_sequence, head_position)| {
                Ok(CrossProcessQueueState {
                    next_sequence: nonnegative_u64(next_sequence, "queue sequence")?,
                    head_position: nonnegative_u64(head_position, "queue position")?,
                })
            })
    }

    pub(super) fn save_queue_state(&self, state: CrossProcessQueueState) -> Result<(), SQLError> {
        self.connection
            .execute(
                "UPDATE queue_state SET next_sequence = ?1, head_position = ?2 WHERE singleton = 1",
                params![
                    sqlite_integer(state.next_sequence, "queue sequence")?,
                    sqlite_integer(state.head_position, "queue position")?,
                ],
            )
            .map_err(|error| registry_error("save queue state", &error))?;
        Ok(())
    }

    pub(super) fn append_entries(
        &self,
        entries: &[CrossProcessQueueEntry],
    ) -> Result<(), SQLError> {
        let mut statement = self
            .connection
            .prepare_cached(
                "INSERT INTO queue_entries (sequence, process_id, channel, payload) VALUES (?1, ?2, ?3, ?4)",
            )
            .map_err(|error| registry_error("prepare queue append", &error))?;
        for entry in entries {
            statement
                .execute(params![
                    sqlite_integer(entry.sequence, "entry sequence")?,
                    entry.process_id,
                    entry.channel,
                    entry.payload,
                ])
                .map_err(|error| registry_error("append queue entry", &error))?;
        }
        Ok(())
    }

    pub(super) fn entries_from(
        &self,
        from_sequence: u64,
    ) -> Result<Vec<CrossProcessQueueEntry>, SQLError> {
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT sequence, process_id, channel, payload FROM queue_entries WHERE sequence >= ?1 ORDER BY sequence",
            )
            .map_err(|error| registry_error("prepare queue scan", &error))?;
        let rows = statement
            .query_map(
                params![sqlite_integer(from_sequence, "scan sequence")?],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i32>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(|error| registry_error("scan queue entries", &error))?;
        rows.map(|row| {
            let (sequence, process_id, channel, payload) =
                row.map_err(|error| registry_error("read queue entry", &error))?;
            Ok(CrossProcessQueueEntry {
                sequence: nonnegative_u64(sequence, "entry sequence")?,
                process_id,
                channel,
                payload,
            })
        })
        .collect()
    }

    pub(super) fn delete_entries_before(&self, sequence: u64) -> Result<(), SQLError> {
        self.connection
            .execute(
                "DELETE FROM queue_entries WHERE sequence < ?1",
                params![sqlite_integer(sequence, "cleanup sequence")?],
            )
            .map_err(|error| registry_error("clean queue entries", &error))?;
        Ok(())
    }

    pub(super) fn listeners(&self) -> Result<Vec<CrossProcessListenerRow>, SQLError> {
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT owner_id, session_id, process_id, wake_port, channels_json, transaction_open, next_sequence, position FROM listeners ORDER BY owner_id, session_id",
            )
            .map_err(|error| registry_error("prepare listener scan", &error))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })
            .map_err(|error| registry_error("scan listeners", &error))?;
        rows.map(|row| {
            let (
                owner_id,
                session_id,
                process_id,
                wake_port,
                channels_json,
                transaction_open,
                next_sequence,
                position,
            ) = row.map_err(|error| registry_error("read listener", &error))?;
            Ok(CrossProcessListenerRow {
                owner_id: fixed_bytes(owner_id, "owner identity")?,
                session_id: u64::from_be_bytes(fixed_bytes(session_id, "session identity")?),
                process_id,
                wake_port: u16::try_from(wake_port).map_err(|_| {
                    SQLError::Internal(format!(
                        "corrupt asynchronous notification wake port {wake_port}"
                    ))
                })?,
                channels: serde_json::from_str(&channels_json).map_err(|error| {
                    SQLError::Internal(format!(
                        "decode asynchronous notification listener channels: {error}"
                    ))
                })?,
                transaction_open,
                next_sequence: nonnegative_u64(next_sequence, "listener sequence")?,
                position: nonnegative_u64(position, "listener position")?,
            })
        })
        .collect()
    }

    pub(super) fn save_listener(&self, listener: &CrossProcessListenerRow) -> Result<(), SQLError> {
        let channels_json = serde_json::to_string(&listener.channels).map_err(|error| {
            SQLError::Internal(format!(
                "encode asynchronous notification listener channels: {error}"
            ))
        })?;
        self.connection
            .execute(
                "INSERT INTO listeners (owner_id, session_id, process_id, wake_port, channels_json, transaction_open, next_sequence, position) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(owner_id, session_id) DO UPDATE SET process_id = excluded.process_id, wake_port = excluded.wake_port, channels_json = excluded.channels_json, transaction_open = excluded.transaction_open, next_sequence = excluded.next_sequence, position = excluded.position",
                params![
                    listener.owner_id.as_slice(),
                    listener.session_id.to_be_bytes().as_slice(),
                    listener.process_id,
                    i64::from(listener.wake_port),
                    channels_json,
                    listener.transaction_open,
                    sqlite_integer(listener.next_sequence, "listener sequence")?,
                    sqlite_integer(listener.position, "listener position")?,
                ],
            )
            .map_err(|error| registry_error("save listener", &error))?;
        Ok(())
    }

    pub(super) fn drop_listener(
        &self,
        owner_id: [u8; 16],
        session_id: u64,
    ) -> Result<(), SQLError> {
        self.connection
            .execute(
                "DELETE FROM listeners WHERE owner_id = ?1 AND session_id = ?2",
                params![owner_id.as_slice(), session_id.to_be_bytes().as_slice()],
            )
            .map_err(|error| registry_error("remove listener", &error))?;
        Ok(())
    }

    pub(super) fn commit(mut self) -> Result<(), SQLError> {
        self.connection
            .execute_batch("COMMIT")
            .map_err(|error| registry_error("commit registry transaction", &error))?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for CrossProcessRegistryTransaction {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.connection.execute_batch("ROLLBACK");
        }
    }
}

pub(super) struct CrossProcessCoordinator {
    database_path: PathBuf,
    registry_path: PathBuf,
    wake_port: u16,
    shutdown: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl CrossProcessCoordinator {
    pub(super) fn allocate_backend_process_id_for_database(
        database_path: &Path,
    ) -> Result<i32, SQLError> {
        let registry_path = suffixed_path(database_path, ".uqa-notification-state");
        initialize_registry(&registry_path).map_err(SQLError::Internal)?;
        let transaction = open_registry_transaction(&registry_path)?;
        let process_id = transaction.allocate_backend_process_id()?;
        transaction.commit()?;
        Ok(process_id)
    }

    pub(super) fn open(database_path: &Path) -> Result<(Self, TcpListener), String> {
        let registry_path = suffixed_path(database_path, ".uqa-notification-state");
        initialize_registry(&registry_path)?;
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .map_err(|error| format!("bind asynchronous notification wake listener: {error}"))?;
        let wake_port = listener
            .local_addr()
            .map_err(|error| format!("read asynchronous notification wake address: {error}"))?
            .port();
        Ok((
            Self {
                database_path: database_path.to_path_buf(),
                registry_path,
                wake_port,
                shutdown: Arc::new(AtomicBool::new(false)),
                worker: Mutex::new(None),
            },
            listener,
        ))
    }

    pub(super) fn start_worker(
        &self,
        listener: TcpListener,
        hub: Weak<NotificationHub>,
    ) -> Result<(), String> {
        let shutdown = Arc::clone(&self.shutdown);
        let worker = std::thread::Builder::new()
            .name("uqa-notification-wake".into())
            .spawn(move || loop {
                match listener.accept() {
                    Ok((stream, _)) => drop(stream),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
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
        open_registry_transaction(&self.registry_path)
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
            if worker.thread().id() != std::thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}

fn initialize_registry(path: &Path) -> Result<(), String> {
    let mut connection = Connection::open(path)
        .map_err(|error| format!("open asynchronous notification registry: {error}"))?;
    connection
        .busy_timeout(REGISTRY_BUSY_TIMEOUT)
        .map_err(|error| format!("set asynchronous notification registry timeout: {error}"))?;
    connection
        .pragma_update(None, "secure_delete", true)
        .map_err(|error| format!("enable asynchronous notification secure deletion: {error}"))?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| {
            format!("begin asynchronous notification registry initialization: {error}")
        })?;
    let version = transaction
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(|error| format!("read asynchronous notification registry version: {error}"))?;
    let application_id = transaction
        .pragma_query_value(None, "application_id", |row| row.get::<_, i64>(0))
        .map_err(|error| {
            format!("read asynchronous notification registry application id: {error}")
        })?;
    match version {
        REGISTRY_SCHEMA_VERSION if application_id == REGISTRY_APPLICATION_ID => {
            validate_registry_schema(&transaction)?;
            transaction.commit().map_err(|error| {
                format!("commit asynchronous notification registry validation: {error}")
            })
        }
        0 if application_id == 0 => {
            transaction
                .execute_batch(
                    "CREATE TABLE queue_state (
                         singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                         next_sequence INTEGER NOT NULL CHECK (next_sequence >= 0),
                         head_position INTEGER NOT NULL CHECK (head_position >= 0)
                     ) STRICT;
                     INSERT INTO queue_state (singleton, next_sequence, head_position)
                     VALUES (1, 0, 0);
                     CREATE TABLE backend_process_id_state (
                         singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                         next_process_id INTEGER NOT NULL CHECK (next_process_id BETWEEN 1 AND 2147483648)
                     ) STRICT;
                     INSERT INTO backend_process_id_state (singleton, next_process_id)
                     VALUES (1, 1);
                     CREATE TABLE queue_entries (
                         sequence INTEGER PRIMARY KEY CHECK (sequence >= 0),
                         process_id INTEGER NOT NULL CHECK (process_id > 0),
                         channel TEXT NOT NULL,
                         payload TEXT NOT NULL
                     ) STRICT;
                     CREATE TABLE listeners (
                         owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
                         session_id BLOB NOT NULL CHECK (length(session_id) = 8),
                         process_id INTEGER NOT NULL CHECK (process_id > 0),
                         wake_port INTEGER NOT NULL CHECK (wake_port BETWEEN 1 AND 65535),
                         channels_json TEXT NOT NULL,
                         transaction_open INTEGER NOT NULL CHECK (transaction_open IN (0, 1)),
                         next_sequence INTEGER NOT NULL CHECK (next_sequence >= 0),
                         position INTEGER NOT NULL CHECK (position >= 0),
                         PRIMARY KEY (owner_id, session_id)
                     ) STRICT;",
                )
                .map_err(|error| {
                    format!("initialize asynchronous notification registry: {error}")
                })?;
            transaction
                .pragma_update(None, "application_id", REGISTRY_APPLICATION_ID)
                .map_err(|error| {
                    format!("identify asynchronous notification registry: {error}")
                })?;
            transaction
                .pragma_update(None, "user_version", REGISTRY_SCHEMA_VERSION)
                .map_err(|error| {
                    format!("version asynchronous notification registry: {error}")
                })?;
            validate_registry_schema(&transaction)?;
            transaction.commit().map_err(|error| {
                format!("commit asynchronous notification registry initialization: {error}")
            })
        }
        REGISTRY_SCHEMA_VERSION => Err(format!(
            "asynchronous notification registry has application id {application_id}, expected {REGISTRY_APPLICATION_ID}"
        )),
        version if version > REGISTRY_SCHEMA_VERSION => Err(format!(
            "asynchronous notification registry schema version {version} is newer than supported version {REGISTRY_SCHEMA_VERSION}"
        )),
        version => Err(format!(
            "asynchronous notification registry has unsupported schema version {version} and application id {application_id}"
        )),
    }
}

fn validate_registry_schema(transaction: &rusqlite::Transaction<'_>) -> Result<(), String> {
    let queue_state_rows = transaction
        .query_row("SELECT count(*) FROM queue_state", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| format!("validate asynchronous notification queue state: {error}"))?;
    if queue_state_rows != 1 {
        return Err(format!(
            "asynchronous notification registry has {queue_state_rows} queue state rows, expected 1"
        ));
    }
    let process_id_state_rows = transaction
        .query_row("SELECT count(*) FROM backend_process_id_state", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| {
            format!("validate asynchronous notification backend process identifier state: {error}")
        })?;
    if process_id_state_rows != 1 {
        return Err(format!(
            "asynchronous notification registry has {process_id_state_rows} backend process identifier state rows, expected 1"
        ));
    }
    for (name, query) in [
        (
            "queue state",
            "SELECT singleton, next_sequence, head_position FROM queue_state LIMIT 0",
        ),
        (
            "backend process identifier state",
            "SELECT singleton, next_process_id FROM backend_process_id_state LIMIT 0",
        ),
        (
            "queue entries",
            "SELECT sequence, process_id, channel, payload FROM queue_entries LIMIT 0",
        ),
        (
            "listeners",
            "SELECT owner_id, session_id, process_id, wake_port, channels_json, transaction_open, next_sequence, position FROM listeners LIMIT 0",
        ),
    ] {
        transaction.prepare(query).map_err(|error| {
            format!("validate asynchronous notification registry {name}: {error}")
        })?;
    }
    Ok(())
}

fn registry_error(action: &str, error: &rusqlite::Error) -> SQLError {
    SQLError::Internal(format!(
        "{action} in asynchronous notification registry: {error}"
    ))
}

fn open_registry_transaction(
    registry_path: &Path,
) -> Result<CrossProcessRegistryTransaction, SQLError> {
    let connection =
        Connection::open(registry_path).map_err(|error| registry_error("open registry", &error))?;
    connection
        .busy_timeout(REGISTRY_BUSY_TIMEOUT)
        .map_err(|error| registry_error("set registry busy timeout", &error))?;
    connection
        .pragma_update(None, "secure_delete", true)
        .map_err(|error| registry_error("enable registry secure deletion", &error))?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| registry_error("begin registry transaction", &error))?;
    Ok(CrossProcessRegistryTransaction {
        connection,
        finished: false,
    })
}

fn sqlite_integer(value: u64, label: &str) -> Result<i64, SQLError> {
    i64::try_from(value).map_err(|_| {
        SQLError::Internal(format!(
            "asynchronous notification {label} {value} exceeds SQLite INTEGER"
        ))
    })
}

fn nonnegative_u64(value: i64, label: &str) -> Result<u64, SQLError> {
    u64::try_from(value).map_err(|_| {
        SQLError::Internal(format!("corrupt asynchronous notification {label} {value}"))
    })
}

fn fixed_bytes<const N: usize>(bytes: Vec<u8>, label: &str) -> Result<[u8; N], SQLError> {
    let length = bytes.len();
    bytes.try_into().map_err(|_| {
        SQLError::Internal(format!(
            "corrupt asynchronous notification {label} has {length} bytes"
        ))
    })
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    #[test]
    fn registry_initialization_serializes_concurrent_first_open() {
        let directory = tempfile::tempdir().unwrap();
        let path = Arc::new(directory.path().join("notification-state"));
        let barrier = Arc::new(Barrier::new(8));
        let workers = (0..8)
            .map(|_| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    initialize_registry(&path)
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        initialize_registry(&path).unwrap();
    }

    #[test]
    fn registry_open_rejects_missing_versioned_state_instead_of_repairing_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notification-state");
        initialize_registry(&path).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch("DROP TABLE listeners")
            .unwrap();
        let error = initialize_registry(&path).unwrap_err();
        assert!(
            error.contains("validate asynchronous notification registry listeners"),
            "{error}"
        );
    }

    #[test]
    fn dropped_registry_transaction_discards_prepared_queue_changes() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("database");
        let (coordinator, _listener) = CrossProcessCoordinator::open(&database).unwrap();
        {
            let transaction = coordinator.begin_registry_transaction().unwrap();
            transaction
                .append_entries(&[CrossProcessQueueEntry {
                    sequence: 0,
                    process_id: 1,
                    channel: "events".into(),
                    payload: "prepared".into(),
                }])
                .unwrap();
            transaction
                .save_queue_state(CrossProcessQueueState {
                    next_sequence: 1,
                    head_position: 20,
                })
                .unwrap();
        }
        let transaction = coordinator.begin_registry_transaction().unwrap();
        assert_eq!(
            transaction.queue_state().unwrap(),
            CrossProcessQueueState::default()
        );
        assert!(transaction.entries_from(0).unwrap().is_empty());
        transaction.commit().unwrap();
    }
}
