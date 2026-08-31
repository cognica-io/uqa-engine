//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped ownership of callback transaction completion and failure cleanup.

use super::{Engine, SQLError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransactionScopeState {
    Active,
    Finished,
}

pub(super) struct TransactionScope<'engine> {
    engine: &'engine Engine,
    depth_before: usize,
    state: TransactionScopeState,
}

impl<'engine> TransactionScope<'engine> {
    pub(super) fn begin(engine: &'engine Engine) -> Result<Self, SQLError> {
        let depth_before = engine.transaction_depth();
        engine.begin()?;
        Ok(Self {
            engine,
            depth_before,
            state: TransactionScopeState::Active,
        })
    }

    pub(super) fn commit(&mut self) -> Result<(), SQLError> {
        let expected_depth = self.depth_before.saturating_add(1);
        let current_depth = self.engine.transaction_depth();
        if current_depth != expected_depth {
            let depth_error = SQLError::Internal(format!(
                "transaction callback changed scoped frame depth from {expected_depth} to {current_depth}"
            ));
            return match self.rollback() {
                Ok(()) => Err(depth_error),
                Err(rollback_error) => Err(SQLError::Internal(format!(
                    "transaction cleanup after an unbalanced callback failed: {rollback_error}; original error: {depth_error}"
                ))),
            };
        }
        match self.engine.commit() {
            Ok(()) => {
                self.finish_if_closed();
                if self.state == TransactionScopeState::Finished {
                    Ok(())
                } else {
                    let depth_error = SQLError::Internal(
                        "transaction commit did not close its scoped frame".into(),
                    );
                    match self.rollback() {
                        Ok(()) => Err(depth_error),
                        Err(rollback_error) => Err(SQLError::Internal(format!(
                            "transaction cleanup after an incomplete commit failed: {rollback_error}; original error: {depth_error}"
                        ))),
                    }
                }
            }
            Err(commit_error) => {
                self.finish_if_closed();
                if self.state == TransactionScopeState::Finished {
                    return Err(commit_error);
                }
                match self.rollback() {
                    Ok(()) => Err(commit_error),
                    Err(rollback_error) => Err(SQLError::Internal(format!(
                        "transaction rollback after commit failure failed: {rollback_error}; original commit error: {commit_error}"
                    ))),
                }
            }
        }
    }

    pub(super) fn rollback(&mut self) -> Result<(), SQLError> {
        let mut first_error = None;
        let mut additional_errors = Vec::new();
        while self.engine.transaction_depth() > self.depth_before {
            let depth_before_rollback = self.engine.transaction_depth();
            if let Err(error) = self.engine.rollback() {
                if first_error.is_none() {
                    first_error = Some(error);
                } else {
                    additional_errors.push(error.to_string());
                }
            }
            if self.engine.transaction_depth() >= depth_before_rollback {
                break;
            }
        }
        self.finish_if_closed();
        if self.state == TransactionScopeState::Active && first_error.is_none() {
            first_error = Some(SQLError::Internal(
                "transaction rollback did not close its scoped frame".into(),
            ));
        }
        match (first_error, additional_errors.is_empty()) {
            (None, _) => Ok(()),
            (Some(error), true) => Err(error),
            (Some(error), false) => Err(SQLError::Internal(format!(
                "{error}; additional transaction rollback failures: {}",
                additional_errors.join("; ")
            ))),
        }
    }

    fn finish_if_closed(&mut self) {
        if self.engine.transaction_depth() <= self.depth_before {
            self.state = TransactionScopeState::Finished;
        }
    }
}

impl Drop for TransactionScope<'_> {
    fn drop(&mut self) {
        if self.state == TransactionScopeState::Active {
            let _ = self.rollback();
        }
    }
}
