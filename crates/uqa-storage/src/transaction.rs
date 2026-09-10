//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-memory transaction snapshots and shared transaction errors.

use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum TransactionError {
    #[error("transaction already finished")]
    Finished,
    #[error("savepoint `{0}` does not exist")]
    UnknownSavepoint(String),
    #[error(transparent)]
    Storage(#[from] crate::StorageBackendError),
}

pub type TxResult<T> = std::result::Result<T, TransactionError>;

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
}
