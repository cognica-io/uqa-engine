//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::io;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use parking_lot::Mutex;
use uqa_core::CancellationToken;
use uqa_engine::Engine;
use uqa_pg_wire::{CancelKey, ProtocolVersion};

use crate::connection;

/// Listener policy. Trust authentication must be selected explicitly by the embedding host.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    pub trust_authentication: bool,
    pub max_message_bytes: usize,
    pub max_protocol_version: ProtocolVersion,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 5433)),
            trust_authentication: false,
            max_message_bytes: uqa_pg_wire::frontend::DEFAULT_MAX_MESSAGE_LEN,
            max_protocol_version: ProtocolVersion::LATEST,
        }
    }
}

pub(crate) struct Client {
    pub secret: CancelKey,
    pub cancellation: CancellationToken,
    pub executing: bool,
}

#[derive(Default)]
pub(crate) struct Shared {
    pub stopping: AtomicBool,
    pub clients: Mutex<BTreeMap<i32, Client>>,
    sockets: Mutex<BTreeMap<u64, TcpStream>>,
}

impl Shared {
    fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        for client in self.clients.lock().values() {
            client.cancellation.cancel();
        }
        for socket in self.sockets.lock().values() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}

struct SocketRegistration<'a> {
    shared: &'a Shared,
    id: u64,
}

impl Drop for SocketRegistration<'_> {
    fn drop(&mut self) {
        self.shared.sockets.lock().remove(&self.id);
    }
}

/// A running listener. Dropping it closes connections, cancels active work, and joins its workers.
pub struct Server {
    address: SocketAddr,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl Server {
    pub fn start(config: ServerConfig, database: Arc<Engine>) -> io::Result<Self> {
        if !config.trust_authentication {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "select an authentication policy before starting the server",
            ));
        }
        config
            .max_protocol_version
            .negotiate_with_max(config.max_protocol_version)
            .map_err(io::Error::other)?;
        let listener = TcpListener::bind(config.listen)?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared::default());
        let context = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("uqa-pg-listener".into())
            .spawn(move || accept_connections(&listener, &config, &database, &context))?;
        Ok(Self {
            address,
            shared,
            worker: Some(worker),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        self.shared.stop();
        match self.worker.take() {
            Some(worker) => worker
                .join()
                .map_err(|_| io::Error::other("PostgreSQL listener panicked"))?,
            None => Ok(()),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn accept_connections(
    listener: &TcpListener,
    config: &ServerConfig,
    database: &Arc<Engine>,
    shared: &Arc<Shared>,
) -> io::Result<()> {
    let mut workers = Vec::new();
    let mut next_socket = 0_u64;
    let result = (|| {
        while !shared.stopping.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((socket, _)) => {
                    socket.set_nodelay(true)?;
                    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
                    next_socket += 1;
                    let socket_id = next_socket;
                    {
                        let mut sockets = shared.sockets.lock();
                        if shared.stopping.load(Ordering::Acquire) {
                            break;
                        }
                        sockets.insert(socket_id, socket.try_clone()?);
                    }
                    let context = Arc::clone(shared);
                    let engine = Arc::clone(database);
                    let policy = config.clone();
                    workers.push(thread::Builder::new().name("uqa-pg-session".into()).spawn(
                        move || {
                            let _registration = SocketRegistration {
                                shared: &context,
                                id: socket_id,
                            };
                            connection::serve(socket, &engine, &policy, &context)
                        },
                    )?);
                    let mut pending = Vec::new();
                    for worker in workers.drain(..) {
                        if worker.is_finished() {
                            let _ = worker.join();
                        } else {
                            pending.push(worker);
                        }
                    }
                    workers = pending;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    })();
    shared.stop();
    for worker in workers {
        let _ = worker.join();
    }
    result
}
