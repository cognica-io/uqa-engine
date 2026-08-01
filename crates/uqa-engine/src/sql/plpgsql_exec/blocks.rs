//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Block execution and subtransaction-backed exception handling.

use super::{
    arm_matches, catchable, routine_message, Flow, Interpreter, PLpgSQLBlock, PLpgSQLStmt, SQLError,
};

impl Interpreter<'_> {
    /// Run one block, routing failures through its EXCEPTION arms.
    pub(super) fn exec_block(&mut self, block: &PLpgSQLBlock) -> Result<Flow, SQLError> {
        let result = if block.exceptions.is_empty() {
            self.exec_stmts(&block.body)
        } else {
            self.exec_exception_block(block)
        };
        match result {
            Ok(Flow::Exit(Some(label))) if block.label.as_deref() == Some(label.as_str()) => {
                Ok(Flow::Normal)
            }
            other => other,
        }
    }

    /// `PostgreSQL` executes the guarded body of a block with `EXCEPTION`
    /// inside a subtransaction. Database changes made before an error are
    /// rolled back before its handler runs, while PL/pgSQL datum values stay
    /// unchanged. The engine's nested transaction frame provides those same
    /// memory-snapshot and persistent-backend savepoint semantics.
    pub(super) fn exec_exception_block(&mut self, block: &PLpgSQLBlock) -> Result<Flow, SQLError> {
        if self.engine.transaction_depth() == 0 {
            return Err(SQLError::Internal(
                "PL/pgSQL exception block executed outside a statement transaction".into(),
            ));
        }
        self.engine.begin()?;
        match self.exec_stmts(&block.body) {
            Ok(flow) => {
                self.engine.commit()?;
                Ok(flow)
            }
            Err(error) => {
                if let Err(rollback_error) = self.engine.rollback() {
                    return Err(SQLError::Internal(format!(
                        "PL/pgSQL exception-block rollback failed: {rollback_error}; original error: {error}"
                    )));
                }
                if !catchable(&error) {
                    return Err(error);
                }
                let state = error
                    .sqlstate()
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "caught PL/pgSQL error has no SQLSTATE: {error}"
                        ))
                    })?
                    .to_string();
                let message = routine_message(&error);
                let mut arm = None;
                for candidate in &block.exceptions {
                    if arm_matches(&candidate.conditions, &state)? {
                        arm = Some(candidate);
                        break;
                    }
                }
                match arm {
                    Some(arm) => {
                        self.err_stack.push((state, message));
                        let handled = self.exec_stmts(&arm.body);
                        self.err_stack.pop();
                        handled
                    }
                    None => Err(error),
                }
            }
        }
    }

    pub(super) fn exec_stmts(&mut self, stmts: &[PLpgSQLStmt]) -> Result<Flow, SQLError> {
        for stmt in stmts {
            match self.exec_stmt(stmt)? {
                Flow::Normal => {}
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Normal)
    }
}
