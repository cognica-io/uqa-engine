//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admission and failure ownership for native prepared requests, including their initial syntax and retained-definition reads.

use super::{Engine, SQLError, TransactionScope, TransactionStatus};
use std::sync::Arc;
use uqa_sql::{semantics::effects::transaction_blocks::statement_exits_transaction, Statement};

pub(crate) enum PreparedOperation {
    Parse {
        statement: Arc<Statement>,
        outermost: bool,
    },
    Describe {
        returns_rows: bool,
        requires_analysis: bool,
        outermost: bool,
    },
    Bind {
        statement: Arc<Statement>,
        parameter_count: usize,
    },
}

impl PreparedOperation {
    fn accepts_failed_transaction(&self) -> bool {
        match self {
            Self::Parse { statement, .. } => {
                matches!(statement.as_ref(), Statement::Empty)
                    || statement_exits_transaction(statement)
            }
            Self::Describe { returns_rows, .. } => !returns_rows,
            Self::Bind {
                statement,
                parameter_count,
            } => *parameter_count == 0 && statement_exits_transaction(statement),
        }
    }

    fn selects_snapshot(&self) -> bool {
        match self {
            Self::Parse {
                statement,
                outermost: true,
            } => uqa_sql::prepared::utility::statement_requires_analysis(statement),
            Self::Describe {
                returns_rows: true,
                requires_analysis,
                outermost: true,
            } => *requires_analysis,
            _ => false,
        }
    }
}

impl Engine {
    /// Inspect syntax or retained identity before conditional admission. The same boundary owns errors and unwinding from inspection, analysis, binding and execution; failed-frame admission never enables user input conversion.
    pub(crate) fn run_prepared_operation<I, R>(
        &self,
        inspect: impl FnOnce() -> Result<(PreparedOperation, I), SQLError>,
        execute: impl FnOnce(I) -> Result<R, SQLError>,
    ) -> Result<R, SQLError> {
        let _statement = self.lock_statement_gate();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (operation, input) = inspect()?;
            let status = self
                .session
                .transactions
                .lock()
                .last()
                .map(|frame| frame.status);
            if let Some(status) = status {
                let failed = matches!(
                    status,
                    TransactionStatus::Failed | TransactionStatus::FailedBackendAborted
                );
                if !(failed && operation.accepts_failed_transaction()) {
                    if let Some(error) = Self::transaction_status_error(status) {
                        return Err(error);
                    }
                }
            }
            if operation.selects_snapshot() && self.query_table_snapshots.is_none() {
                if status.is_some() {
                    self.prepare_explicit_statement_snapshot(true)?;
                    self.mark_transaction_snapshot_set();
                } else if self
                    .storage
                    .backend
                    .as_ref()
                    .is_some_and(|backend| !backend.in_transaction())
                {
                    let scope = TransactionScope::begin_implicit_statement(self, true)?;
                    return self.run_transaction_scope(
                        scope,
                        |engine| {
                            engine.prepare_explicit_statement_snapshot(true)?;
                            engine.mark_transaction_snapshot_set();
                            execute(input)
                        },
                        std::convert::identity,
                    );
                }
            }
            execute(input)
        }));
        self.finish_transaction_operation(outcome, std::convert::identity)
    }
}
