//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Procedural transaction boundaries for nonatomic `CALL` and `DO` execution.

use super::{routine::routine_transaction_control_allowed, Flow, Interpreter, SQLError};

impl Interpreter<'_> {
    pub(super) fn exec_procedural_transaction(
        &mut self,
        commit: bool,
        chain: bool,
    ) -> Result<Flow, SQLError> {
        if !routine_transaction_control_allowed(self.engine) {
            return Err(invalid_transaction_termination());
        }
        self.engine
            .finish_procedural_transaction(commit, chain)
            .map(|()| Flow::Normal)
    }
}

fn invalid_transaction_termination() -> SQLError {
    SQLError::Routine {
        sqlstate: "2D000".into(),
        message: "invalid transaction termination".into(),
    }
}
