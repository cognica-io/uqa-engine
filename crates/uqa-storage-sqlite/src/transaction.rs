//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` transactions with automatic rollback on drop.

use crate::{ManagedConnection, SQLiteError};
use uqa_storage::{TransactionError, TxResult};

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

#[cfg(test)]
mod tests {
    use super::*;
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
