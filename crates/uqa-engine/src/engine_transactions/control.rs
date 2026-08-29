//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{failed_transaction_error, Engine, SQLError, TransactionFrame, TransactionStatus};
use uqa_sql::ast::TransactionStmt;

impl Engine {
    pub fn run_transaction_statement(&self, tx: TransactionStmt) -> Result<(), SQLError> {
        let _statement = self.runtime.statement_gate.lock();
        let apply_on_commit = self.prepare_transaction_completion(&tx)?;
        let mut guard = self.session.transactions.lock();
        let failed = guard
            .last()
            .is_some_and(|frame| frame.status != TransactionStatus::Active);
        let outcome = self.dispatch_transaction_statement(&mut guard, tx, failed, apply_on_commit);
        match outcome {
            Ok(()) => Ok(()),
            Err(error) => {
                // PostgreSQL aborts the enclosing transaction when a savepoint command or nested BEGIN fails; later statements see 25P02 until ROLLBACK. Frames that this failing command could not open or that no longer exist need no abort.
                let still_open = guard
                    .last()
                    .is_some_and(|frame| frame.status == TransactionStatus::Active);
                drop(guard);
                if still_open {
                    return Err(self.abort_sql_transaction_after_error(error));
                }
                Err(error)
            }
        }
    }

    fn prepare_transaction_completion(&self, tx: &TransactionStmt) -> Result<bool, SQLError> {
        let apply_on_commit = if matches!(
            tx,
            TransactionStmt::Commit | TransactionStmt::CommitAndChain
        ) {
            let stack = self.session.transactions.lock();
            stack.len() == 1
                && stack
                    .last()
                    .is_some_and(|frame| frame.status == TransactionStatus::Active)
        } else {
            false
        };
        if apply_on_commit {
            let validation = {
                let mut stack = self.session.transactions.lock();
                self.validate_deferred_constraints_before_commit(&mut stack, false)
            };
            validation?;
            if let Err(error) = self.apply_temporary_on_commit_actions() {
                return Err(self.abort_sql_transaction_after_error(error));
            }
        }
        Ok(apply_on_commit)
    }

    fn dispatch_transaction_statement(
        &self,
        guard: &mut Vec<TransactionFrame>,
        tx: TransactionStmt,
        failed: bool,
        apply_on_commit: bool,
    ) -> Result<(), SQLError> {
        match tx {
            TransactionStmt::Rollback => self.rollback_transaction_frame(guard),
            TransactionStmt::Commit if failed => self.rollback_transaction_frame(guard),
            TransactionStmt::CommitAndChain if failed => {
                self.finish_transaction_and_chain(guard, "COMMIT", false, false)
            }
            TransactionStmt::CommitAndChain => {
                self.finish_transaction_and_chain(guard, "COMMIT", true, apply_on_commit)
            }
            TransactionStmt::RollbackAndChain => {
                self.finish_transaction_and_chain(guard, "ROLLBACK", false, false)
            }
            TransactionStmt::RollbackToSavepoint(name) => {
                self.rollback_to_transaction_savepoint(guard, &name)
            }
            _ if failed => Err(failed_transaction_error()),
            TransactionStmt::Begin => {
                let characteristics = self.transaction_characteristics_for_begin(
                    guard,
                    uqa_sql::ast::TransactionCharacteristics::default(),
                );
                let outer = guard.is_empty();
                self.begin_transaction_frame(
                    guard,
                    characteristics.read_only,
                    outer,
                    false,
                    characteristics,
                )
            }
            TransactionStmt::BeginWithCharacteristics(options) => {
                let characteristics = self.transaction_characteristics_for_begin(guard, options);
                let outer = guard.is_empty();
                self.begin_transaction_frame(
                    guard,
                    characteristics.read_only,
                    outer,
                    false,
                    characteristics,
                )
            }
            TransactionStmt::Commit => self.commit_transaction_frame(guard, apply_on_commit),
            TransactionStmt::SetCharacteristics(options) => {
                Self::apply_transaction_characteristics(guard, options)
            }
            TransactionStmt::SetSessionCharacteristics(options) => {
                self.set_session_transaction_characteristics(options);
                Ok(())
            }
            TransactionStmt::SetSnapshot(snapshot) => {
                Self::set_transaction_snapshot(guard, &snapshot)
            }
            TransactionStmt::Savepoint(name) => self.save_transaction_savepoint(guard, name),
            TransactionStmt::ReleaseSavepoint(name) => {
                self.release_transaction_savepoint(guard, &name)
            }
        }
    }

    fn finish_transaction_and_chain(
        &self,
        stack: &mut Vec<TransactionFrame>,
        command: &str,
        commit: bool,
        deferred_constraints_validated: bool,
    ) -> Result<(), SQLError> {
        let characteristics = stack
            .last()
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "25P01".into(),
                message: format!("{command} AND CHAIN can only be used in transaction blocks"),
            })?
            .characteristics;
        if commit {
            self.commit_transaction_frame(stack, deferred_constraints_validated)?;
        } else {
            self.rollback_transaction_frame(stack)?;
        }
        self.begin_transaction_frame(
            stack,
            characteristics.read_only,
            true,
            false,
            characteristics,
        )
    }

    fn apply_temporary_on_commit_actions(&self) -> Result<(), SQLError> {
        let actions = self
            .storage
            .tables
            .read()
            .iter()
            .filter(|(_, table)| {
                table.persistence == uqa_sql::ast::RelationPersistence::Temporary
                    && table.on_commit != uqa_sql::ast::OnCommitAction::PreserveRows
            })
            .map(|(relation, table)| (relation.qualified_name(), table.on_commit))
            .collect::<Vec<_>>();
        for (name, action) in actions {
            match action {
                uqa_sql::ast::OnCommitAction::PreserveRows => {}
                uqa_sql::ast::OnCommitAction::DeleteRows => {
                    self.truncate_locked_table(&name, false)?;
                }
                uqa_sql::ast::OnCommitAction::Drop => {
                    self.drop_temporary_table_on_commit_inner(&name)
                        .map_err(|error| {
                            SQLError::Internal(format!(
                                "drop temporary table `{name}` at commit: {error}"
                            ))
                        })?;
                }
            }
        }
        Ok(())
    }
}
