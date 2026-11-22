//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ManagedConnection;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Work,
    Shutdown,
    Retry,
    Complete,
    Error(String),
}

// Only the worker owns the sender, so observers also see worker exit or panic.
#[derive(Default)]
pub(super) struct Events(Mutex<Option<Sender<Event>>>);

impl Events {
    fn subscribe(checkpoint: &Checkpoint) -> Receiver<Event> {
        let (sender, receiver) = mpsc::channel();
        let events = checkpoint.events.upgrade().expect("live checkpoint worker");
        assert!(events.0.lock().replace(sender).is_none());
        receiver
    }

    fn send(&self, event: Event) {
        if let Some(sender) = self.0.lock().as_ref() {
            let _ = sender.send(event);
        }
    }

    pub(super) fn enter<'a>(
        &self,
        signal: &'a Signal,
        stopping: bool,
    ) -> parking_lot::MutexGuard<'a, ()> {
        if stopping {
            self.send(Event::Shutdown);
            signal.shutdown_gate.lock()
        } else {
            self.send(Event::Work);
            signal.work_gate.lock()
        }
    }

    pub(super) fn finished(&self, result: &Result<bool>) {
        self.send(match result {
            Ok(true) => Event::Retry,
            Ok(false) => Event::Complete,
            Err(error) => Event::Error(error.to_string()),
        });
    }
}

fn next_event(events: &Receiver<Event>) -> Event {
    match events
        .recv()
        .expect("checkpoint worker exited before reporting progress")
    {
        Event::Error(error) => panic!("checkpoint failed: {error}"),
        event => event,
    }
}

fn wait_for_completion(events: &Receiver<Event>) {
    loop {
        match next_event(events) {
            Event::Complete => return,
            Event::Work | Event::Retry => {}
            event => panic!("expected checkpoint completion, received {event:?}"),
        }
    }
}

#[test]
fn wal_commits_finish_while_maintenance_is_paused_and_shutdown_joins_it() {
    for encrypted in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("checkpoint.db");
        let connection = if encrypted {
            ManagedConnection::open_encrypted(&path, "checkpoint-test-key").unwrap()
        } else {
            ManagedConnection::open(&path).unwrap()
        };
        let checkpoint = connection.pool.checkpoint.as_ref().unwrap();
        let events = Events::subscribe(checkpoint);
        let worker_events = checkpoint.events.clone();
        let signal = Arc::clone(&checkpoint.signal);
        let paused = signal.work_gate.lock();
        connection.with_physical(|sqlite| {
            sqlite.pragma_update(None, "synchronous", "FULL")?;
            sqlite.execute_batch("CREATE TABLE payload(value BLOB); INSERT INTO payload VALUES(zeroblob(8388608));")?;
            assert_eq!(sqlite.pragma_query_value(None, "synchronous", |row| row.get::<_, i64>(0))?, 2);
            Ok(())
        }).unwrap();
        assert_eq!(next_event(&events), Event::Work);
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
        // The committed payload is visible even though no maintenance can run.
        assert_eq!(
            connection
                .new_session()
                .with_physical(|sqlite| {
                    Ok(
                        sqlite.query_row("SELECT length(value) FROM payload", [], |row| {
                            row.get::<_, i64>(0)
                        })?,
                    )
                })
                .unwrap(),
            8 << 20
        );
        drop(paused);
        wait_for_completion(&events);
        std::thread::scope(|threads| {
            let shutdown_gate = signal.shutdown_gate.lock();
            let closing = threads.spawn(move || {
                drop(connection);
                assert!(
                    worker_events.upgrade().is_none(),
                    "checkpoint worker outlived the pool"
                );
            });
            while next_event(&events) != Event::Shutdown {}
            assert!(
                !closing.is_finished(),
                "pool closed while its worker was paused"
            );
            drop(shutdown_gate);
            closing.join().unwrap();
        });
        assert_eq!(events.try_recv(), Ok(Event::Complete));
        assert_eq!(events.try_recv(), Err(TryRecvError::Disconnected));
        let reopened = if encrypted {
            ManagedConnection::open_encrypted(&path, "checkpoint-test-key").unwrap()
        } else {
            ManagedConnection::open(&path).unwrap()
        };
        reopened
            .with_physical(|sqlite| {
                assert_eq!(
                    sqlite.query_row("SELECT length(value) FROM payload", [], |row| row
                        .get::<_, i64>(0))?,
                    8 << 20
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn passive_maintenance_retries_after_a_physical_reader_releases_its_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reader.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let events = Events::subscribe(connection.pool.checkpoint.as_ref().unwrap());
    connection
        .with_physical(|sqlite| {
            sqlite.execute_batch(
                "CREATE TABLE payload(value BLOB); INSERT INTO payload VALUES(zeroblob(1));",
            )?;
            Ok(())
        })
        .unwrap();
    let reader = Connection::open(&path).unwrap();
    reader
        .execute_batch("BEGIN; SELECT * FROM payload;")
        .unwrap();
    connection
        .with_physical(|sqlite| {
            sqlite.execute_batch("UPDATE payload SET value=zeroblob(8388608)")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        reader
            .query_row("SELECT length(value) FROM payload", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(next_event(&events), Event::Work);
    assert_eq!(next_event(&events), Event::Retry);
    reader.execute_batch("ROLLBACK").unwrap();
    wait_for_completion(&events);
}
