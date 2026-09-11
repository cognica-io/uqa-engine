//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transaction commands that require an explicit SQL block.

use crate::SQLError;

pub fn transaction_requires_explicit_block(transaction: &crate::ast::TransactionStmt) -> bool {
    matches!(
        transaction,
        crate::ast::TransactionStmt::Savepoint(_)
            | crate::ast::TransactionStmt::ReleaseSavepoint(_)
            | crate::ast::TransactionStmt::RollbackToSavepoint(_)
            | crate::ast::TransactionStmt::CommitAndChain
            | crate::ast::TransactionStmt::RollbackAndChain
    )
}

pub fn no_active_transaction_error(transaction: &crate::ast::TransactionStmt) -> SQLError {
    let command = match transaction {
        crate::ast::TransactionStmt::Savepoint(_) => "SAVEPOINT",
        crate::ast::TransactionStmt::ReleaseSavepoint(_) => "RELEASE SAVEPOINT",
        crate::ast::TransactionStmt::RollbackToSavepoint(_) => "ROLLBACK TO SAVEPOINT",
        crate::ast::TransactionStmt::CommitAndChain => "COMMIT AND CHAIN",
        crate::ast::TransactionStmt::RollbackAndChain => "ROLLBACK AND CHAIN",
        _ => unreachable!("only explicit-block transaction commands use this error"),
    };
    SQLError::Routine {
        sqlstate: "25P01".into(),
        message: format!("{command} can only be used in transaction blocks"),
    }
}

#[cfg(test)]
mod tests {
    use super::{no_active_transaction_error, transaction_requires_explicit_block};
    use crate::{ast::TransactionStmt, SQLError};

    #[test]
    fn explicit_block_commands_keep_their_individual_diagnostics() {
        for (command, spelling) in [
            (TransactionStmt::Savepoint("s".into()), "SAVEPOINT"),
            (
                TransactionStmt::ReleaseSavepoint("s".into()),
                "RELEASE SAVEPOINT",
            ),
            (
                TransactionStmt::RollbackToSavepoint("s".into()),
                "ROLLBACK TO SAVEPOINT",
            ),
            (TransactionStmt::CommitAndChain, "COMMIT AND CHAIN"),
            (TransactionStmt::RollbackAndChain, "ROLLBACK AND CHAIN"),
        ] {
            assert!(transaction_requires_explicit_block(&command));
            assert!(
                matches!(no_active_transaction_error(&command), SQLError::Routine { sqlstate, message } if sqlstate == "25P01" && message == format!("{spelling} can only be used in transaction blocks"))
            );
        }
        for command in [
            TransactionStmt::Begin,
            TransactionStmt::Commit,
            TransactionStmt::Rollback,
        ] {
            assert!(!transaction_requires_explicit_block(&command));
        }
    }
}
