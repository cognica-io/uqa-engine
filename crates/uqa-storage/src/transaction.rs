//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit user transactions with savepoint stacks.
//!
//! Transaction state and savepoint management.
//!
//! Two flavours:
//! * [`SQLiteTransaction`] -- wraps a [`ManagedConnection`] and routes
//!   begin / commit / rollback / savepoint calls to the underlying
//!   connection. Auto-rollback on drop guards the `SQLite` write log
//!   against in-flight panics.
//! * [`InMemoryTransaction`] -- snapshots the per-table mutable
//!   document store (or whatever the supplied [`Snapshotable`]
//!   impl returns) and rolls it back on demand. Savepoints push
//!   nested snapshots onto a stack.
//!
//! Both expose the same `commit` / `rollback` / `savepoint` /
//! `release_savepoint` / `rollback_to` methods so the engine can
//! treat them uniformly.

use std::collections::BTreeMap;

use crate::sqlite::connection::ManagedConnection;
use crate::SQLiteError;

#[derive(Debug, thiserror::Error)]
pub enum TransactionError {
    #[error("transaction already finished")]
    Finished,
    #[error("savepoint `{0}` does not exist")]
    UnknownSavepoint(String),
    #[error(transparent)]
    Storage(#[from] SQLiteError),
}

pub type TxResult<T> = std::result::Result<T, TransactionError>;

/// SQLite-backed transaction. Drops without commit roll back so
/// panics never leak a half-applied write log.
pub struct SQLiteTransaction {
    conn: ManagedConnection,
    finished: bool,
}

impl SQLiteTransaction {
    pub fn begin(conn: ManagedConnection) -> Result<Self, SQLiteError> {
        conn.begin_transaction()?;
        Ok(Self {
            conn,
            finished: false,
        })
    }

    pub fn active(&self) -> bool {
        !self.finished
    }

    pub fn commit(&mut self) -> TxResult<()> {
        if self.finished {
            return Err(TransactionError::Finished);
        }
        let result = self
            .conn
            .commit_transaction()
            .map_err(TransactionError::from);
        if result.is_ok() || !self.conn.in_transaction() {
            self.finished = true;
        }
        result
    }

    pub fn rollback(&mut self) -> TxResult<()> {
        if self.finished {
            return Err(TransactionError::Finished);
        }
        let result = self
            .conn
            .rollback_transaction()
            .map_err(TransactionError::from);
        if result.is_ok() || !self.conn.in_transaction() {
            self.finished = true;
        }
        result
    }

    pub fn savepoint(&self, name: &str) -> TxResult<()> {
        if self.finished {
            return Err(TransactionError::Finished);
        }
        self.conn.savepoint(name)?;
        Ok(())
    }

    pub fn release_savepoint(&self, name: &str) -> TxResult<()> {
        if self.finished {
            return Err(TransactionError::Finished);
        }
        self.conn.release_savepoint(name)?;
        Ok(())
    }

    pub fn rollback_to(&self, name: &str) -> TxResult<()> {
        if self.finished {
            return Err(TransactionError::Finished);
        }
        self.conn.rollback_to_savepoint(name)?;
        Ok(())
    }
}

impl Drop for SQLiteTransaction {
    fn drop(&mut self) {
        if !self.finished {
            self.conn.rollback_transaction_on_drop();
        }
    }
}

/// Sources that can snapshot themselves and restore the snapshot.
/// The engine implements this for its in-memory table state.
pub trait Snapshotable {
    type Snapshot;
    fn snapshot(&self) -> Self::Snapshot;
    fn restore(&self, snapshot: &Self::Snapshot);
}

/// Pure-in-memory transaction. Snapshots the source via
/// [`Snapshotable::snapshot`] on `begin`, restores via
/// [`Snapshotable::restore`] on rollback.
pub struct InMemoryTransaction<S: Snapshotable> {
    source: S,
    snapshot: Option<S::Snapshot>,
    savepoints: BTreeMap<String, S::Snapshot>,
    finished: bool,
}

impl<S: Snapshotable> InMemoryTransaction<S> {
    pub fn begin(source: S) -> Self {
        let snapshot = source.snapshot();
        Self {
            source,
            snapshot: Some(snapshot),
            savepoints: BTreeMap::new(),
            finished: false,
        }
    }

    pub fn active(&self) -> bool {
        !self.finished
    }

    pub fn commit(&mut self) -> TxResult<()> {
        if self.finished {
            return Err(TransactionError::Finished);
        }
        self.snapshot = None;
        self.savepoints.clear();
        self.finished = true;
        Ok(())
    }

    pub fn rollback(&mut self) -> TxResult<()> {
        if self.finished {
            return Err(TransactionError::Finished);
        }
        if let Some(snap) = self.snapshot.take() {
            self.source.restore(&snap);
        }
        self.savepoints.clear();
        self.finished = true;
        Ok(())
    }

    pub fn savepoint(&mut self, name: impl Into<String>) -> TxResult<()> {
        if self.finished {
            return Err(TransactionError::Finished);
        }
        self.savepoints.insert(name.into(), self.source.snapshot());
        Ok(())
    }

    pub fn release_savepoint(&mut self, name: &str) -> TxResult<()> {
        if self.finished {
            return Err(TransactionError::Finished);
        }
        self.savepoints.remove(name);
        Ok(())
    }

    pub fn rollback_to(&mut self, name: &str) -> TxResult<()> {
        if self.finished {
            return Err(TransactionError::Finished);
        }
        let snap = self
            .savepoints
            .get(name)
            .ok_or_else(|| TransactionError::UnknownSavepoint(name.to_string()))?;
        self.source.restore(snap);
        Ok(())
    }
}

impl<S: Snapshotable> Drop for InMemoryTransaction<S> {
    fn drop(&mut self) {
        if !self.finished {
            if let Some(snap) = self.snapshot.take() {
                self.source.restore(&snap);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Counter source: the snapshot is just the counter value at
    /// snapshot time. `restore` overwrites the live counter.
    #[derive(Clone)]
    struct CounterSource {
        v: Rc<RefCell<i64>>,
    }
    impl Snapshotable for CounterSource {
        type Snapshot = i64;
        fn snapshot(&self) -> i64 {
            *self.v.borrow()
        }
        fn restore(&self, snap: &i64) {
            *self.v.borrow_mut() = *snap;
        }
    }

    #[test]
    fn rollback_restores_state() {
        let v = Rc::new(RefCell::new(10));
        let src = CounterSource { v: v.clone() };
        let mut tx = InMemoryTransaction::begin(src);
        *v.borrow_mut() = 99;
        tx.rollback().unwrap();
        assert_eq!(*v.borrow(), 10);
    }

    #[test]
    fn commit_keeps_changes() {
        let v = Rc::new(RefCell::new(10));
        let src = CounterSource { v: v.clone() };
        let mut tx = InMemoryTransaction::begin(src);
        *v.borrow_mut() = 99;
        tx.commit().unwrap();
        assert_eq!(*v.borrow(), 99);
    }

    #[test]
    fn savepoint_rollback_undoes_partial_writes() {
        let v = Rc::new(RefCell::new(0));
        let src = CounterSource { v: v.clone() };
        let mut tx = InMemoryTransaction::begin(src);
        *v.borrow_mut() = 1;
        tx.savepoint("sp").unwrap();
        *v.borrow_mut() = 2;
        tx.rollback_to("sp").unwrap();
        assert_eq!(*v.borrow(), 1);
        tx.commit().unwrap();
    }

    #[test]
    fn drop_without_commit_rolls_back() {
        let v = Rc::new(RefCell::new(10));
        {
            let src = CounterSource { v: v.clone() };
            let _tx = InMemoryTransaction::begin(src);
            *v.borrow_mut() = 50;
        }
        assert_eq!(*v.borrow(), 10);
    }

    #[test]
    fn unknown_savepoint_errors() {
        let v = Rc::new(RefCell::new(0));
        let src = CounterSource { v };
        let mut tx = InMemoryTransaction::begin(src);
        let err = tx.rollback_to("missing").unwrap_err();
        assert!(matches!(err, TransactionError::UnknownSavepoint(_)));
        tx.commit().unwrap();
    }

    #[test]
    fn sqlite_transaction_commits_writes() {
        let conn = ManagedConnection::open_in_memory().unwrap();
        conn.with(|c| {
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", [])?;
            Ok(())
        })
        .unwrap();
        let mut tx = SQLiteTransaction::begin(conn.clone()).unwrap();
        conn.with(|c| {
            c.execute("INSERT INTO t (id, v) VALUES (1, 'hi')", [])?;
            Ok(())
        })
        .unwrap();
        tx.commit().unwrap();
        let got: i64 = conn
            .with(|c| Ok(c.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(got, 1);
    }

    #[test]
    fn sqlite_transaction_rolls_back_on_drop() {
        let conn = ManagedConnection::open_in_memory().unwrap();
        conn.with(|c| {
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", [])?;
            Ok(())
        })
        .unwrap();
        {
            let _tx = SQLiteTransaction::begin(conn.clone()).unwrap();
            conn.with(|c| {
                c.execute("INSERT INTO t (id, v) VALUES (1, 'hi')", [])?;
                Ok(())
            })
            .unwrap();
            // Tx drops without commit -> rollback fires automatically.
        }
        let got: i64 = conn
            .with(|c| Ok(c.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(got, 0);
    }
}
