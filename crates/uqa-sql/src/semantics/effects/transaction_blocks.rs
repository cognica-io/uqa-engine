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

/// The warning of `command`, a command whose effect lasts only until the end of its transaction, outside a transaction block, as `PostgreSQL`'s `WarnNoTransactionBlock` reports it: such a command lasts only for its own statement. A command inside a function, or inside a block, has a transaction to last for and is not warned about.
pub fn no_transaction_block_warning(command: &str) -> crate::SQLNotice {
    crate::SQLNotice::warning(format!("{command} can only be used in transaction blocks"))
        .with_sqlstate("25P01")
}

/// The warning of a `BEGIN` inside a transaction block, or inside one of its subtransactions, which `PostgreSQL`'s `BeginTransactionBlock` reports and otherwise ignores.
pub fn transaction_in_progress_warning() -> crate::SQLNotice {
    crate::SQLNotice::warning("there is already a transaction in progress").with_sqlstate("25001")
}

/// The warning of a `COMMIT` or `ROLLBACK` outside a transaction block, which `PostgreSQL`'s `EndTransactionBlock` and `UserAbortTransactionBlock` report and otherwise ignore.
pub fn no_transaction_in_progress_warning() -> crate::SQLNotice {
    crate::SQLNotice::warning("there is no transaction in progress").with_sqlstate("25P01")
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
