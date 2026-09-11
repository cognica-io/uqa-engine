//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    row_locks::retry_cache::RowLockRetryCache,
    statement::{
        context::{
            StatementEffects, StatementExecutionContext, StatementExecutionInputs, StatementRuntime,
        },
        transactions::StatementTransactions,
    },
};
use context::{
    BatchRowLocks, BatchTransactions, CachedStatement, RowLockStatementGuard, StatementCache,
};
use parking_lot::Mutex;
use std::cell::{Cell, RefCell};
use uqa_core::CancellationToken;
use uqa_sql::{
    ast::TransactionStmt,
    plan::{AggregateClassifier, ExecutablePlanOptimizer},
    Statement,
};

#[derive(Default)]
struct Inputs {
    cancellation: CancellationToken,
    notices: Mutex<Vec<(String, String)>>,
    events: RefCell<Vec<String>>,
    depth: Cell<usize>,
    guarded: Cell<bool>,
    snapshot: Cell<bool>,
    cached: Option<(Arc<Statement>, Arc<UnifiedPlan>)>,
    cancel_after_cache_lookup: bool,
}

impl Inputs {
    fn context(&self) -> BatchExecutionContext<'_, ()> {
        BatchExecutionContext {
            runtime: StatementRuntime {
                cancellation: &self.cancellation,
                notices: &self.notices,
            },
            persistent_backend: true,
            statements: self,
            cache: self,
            aggregates: self,
            effects: self,
            planning: self,
            transactions: self,
            row_locks: self,
        }
    }
    fn record(&self, event: impl Into<String>) {
        self.events.borrow_mut().push(event.into());
    }
    fn assert_events(&self, events: &[&str]) {
        assert_eq!(&*self.events.borrow(), events);
    }
}

impl StatementExecutionInputs<()> for Inputs {
    fn statement_execution_context(&self) -> StatementExecutionContext<'_, ()> {
        panic!("a rejected batch must not capture execution inputs")
    }
}
impl StatementEffects for Inputs {
    fn query_effect_context(&self) -> uqa_sql::semantics::effects::QueryEffectContext<'_> {
        panic!("transaction and mutation commands need no query-effect lookup")
    }
}
impl AggregateClassifier for Inputs {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}
impl ExecutablePlanOptimizer for Inputs {
    fn plan_for_execution(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<UnifiedPlan, SQLError> {
        assert!(
            matches!(plan, UnifiedPlan::Command(command) if matches!(*command, uqa_sql::plan::CommandPlan::Delete(_)))
        );
        assert!(params.is_empty());
        self.record(format!("optimize.{}", self.snapshot.get()));
        Err(SQLError::Internal("injected planning failure".into()))
    }
}
impl StatementCache for Inputs {
    fn cached_sql_statement(&self, _: &str) -> Option<CachedStatement> {
        self.record("cache.lookup");
        if self.cancel_after_cache_lookup {
            self.cancellation.cancel();
        }
        self.cached
            .as_ref()
            .map(|(statement, logical_plan)| CachedStatement {
                statement: Arc::clone(statement),
                logical_plan: Arc::clone(logical_plan),
                optimized_plan: None,
            })
    }
    fn cached_optimized_sql_plan(&self, _: &str) -> Option<Arc<UnifiedPlan>> {
        panic!("persistent statements cannot use the memory-only cache shortcut")
    }
    fn cache_sql_statement(&self, _: String, _: Arc<Statement>, _: Arc<UnifiedPlan>) {
        self.record(format!("cache.write.{}", self.snapshot.get()));
    }
    fn cache_optimized_sql_plan(&self, _: &str, _: Arc<UnifiedPlan>) {
        panic!("a rejected plan cannot enter the optimized cache")
    }
}
impl StatementTransactions for Inputs {
    fn transaction_depth(&self) -> usize {
        self.depth.get()
    }
    fn current_transaction_is_read_only(&self) -> bool {
        panic!("transaction validation belongs to the later statement executor")
    }
    fn mark_transaction_snapshot_set(&self) {
        panic!("transaction validation belongs to the later statement executor")
    }
    fn abort_after_error(&self, error: SQLError) -> SQLError {
        self.record(format!("abort.{}", self.guarded.get()));
        error
    }
    fn rollback(&self) -> Result<(), SQLError> {
        self.record(format!("rollback.{}", self.guarded.get()));
        assert_ne!(self.depth.replace(0), 0);
        Ok(())
    }
}
impl BatchTransactions for Inputs {
    fn begin_simple_query_transaction(&self) -> Result<(), SQLError> {
        self.record("begin.segment");
        assert_eq!(self.depth.replace(1), 0);
        Ok(())
    }
    fn promote_simple_query_transaction(&self) -> Result<(), SQLError> {
        panic!("a rejected command cannot reach a following BEGIN")
    }
    fn run_transaction_statement(&self, _: TransactionStmt) -> Result<(), SQLError> {
        panic!("a rejected statement cannot commit")
    }
    fn ensure_transaction_usable(&self) -> Result<(), SQLError> {
        self.record("usable");
        Ok(())
    }
    fn prepare_explicit_statement_snapshot(&self, sets_snapshot: bool) -> Result<(), SQLError> {
        self.record(format!("snapshot.{sets_snapshot}"));
        self.snapshot.set(true);
        Ok(())
    }
    fn prepare_explicit_transaction_writer(&self) -> Result<bool, SQLError> {
        panic!("commands do not take the query writer-promotion path")
    }
    fn begin_implicit_statement_transaction(&self, read_only: bool) -> Result<(), SQLError> {
        self.record(format!("begin.statement.{read_only}"));
        assert_eq!(self.depth.replace(1), 0);
        self.snapshot.set(true);
        Ok(())
    }
}
struct Guard<'a>(&'a Inputs);
impl RowLockStatementGuard for Guard<'_> {}
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        assert!(self.0.guarded.replace(false));
        self.0.record("guard.drop");
    }
}
impl BatchRowLocks for Inputs {
    fn begin_row_lock_statement(&self) -> Box<dyn RowLockStatementGuard + '_> {
        assert!(!self.guarded.replace(true));
        self.record("guard.enter");
        Box::new(Guard(self))
    }
    fn statement_row_lock_cache(&self) -> Result<Arc<RowLockRetryCache>, SQLError> {
        panic!("a DELETE command creates its row-lock cache during execution")
    }
}

#[test]
fn cancellation_precedes_cache_and_parse_and_aborts_an_open_transaction() {
    let inputs = Inputs::default();
    inputs.depth.set(1);
    inputs.cancellation.cancel();
    let error = execute(&inputs.context(), "invalid syntax", &[]).unwrap_err();
    assert!(error.to_string().contains("canceling statement"));
    inputs.assert_events(&["abort.false"]);
}

#[test]
fn cancellation_after_cache_lookup_prevents_first_statement_transaction() {
    let inputs = Inputs {
        cancel_after_cache_lookup: true,
        ..Inputs::default()
    };
    let error = execute(
        &inputs.context(),
        "DELETE FROM items; DELETE FROM items",
        &[],
    )
    .unwrap_err();
    assert!(error.to_string().contains("canceling statement"));
    inputs.assert_events(&["cache.lookup"]);
    assert_eq!(inputs.depth.get(), 0);
}

#[test]
fn whole_batch_syntax_failure_precedes_all_commands_and_callbacks() {
    let inputs = Inputs::default();
    let mut delivered = 0;
    assert!(execute_simple_query(
        &inputs.context(),
        "COMMIT; SELECT (",
        &[],
        false,
        &mut |_| {
            delivered += 1;
            Ok(())
        }
    )
    .is_err());
    inputs.assert_events(&["cache.lookup"]);
    assert_eq!(delivered, 0);
    assert!(inputs.notices.lock().is_empty());
    assert_eq!(inputs.depth.get(), 0);
}

#[test]
fn block_only_command_is_rejected_before_opening_an_implicit_segment() {
    let inputs = Inputs::default();
    let error = execute(&inputs.context(), "SAVEPOINT earlier; SELECT 1", &[]).unwrap_err();
    assert!(matches!(error, SQLError::Routine { sqlstate, .. } if sqlstate == "25P01"));
    inputs.assert_events(&["cache.lookup"]);
    assert_eq!(inputs.depth.get(), 0);
}

#[test]
fn out_of_block_completions_and_notices_keep_statement_order() {
    let inputs = Inputs::default();
    let mut tags = Vec::new();
    execute_simple_query(
        &inputs.context(),
        "COMMIT; ROLLBACK",
        &[],
        false,
        &mut |result| {
            tags.push(result.command_tag.clone().unwrap());
            assert_eq!(inputs.notices.lock().len(), tags.len());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(tags, ["COMMIT", "ROLLBACK"]);
    assert_eq!(
        &*inputs.notices.lock(),
        &vec![
            (
                "WARNING".into(),
                "there is no transaction in progress".into()
            );
            2
        ]
    );
    inputs.assert_events(&["cache.lookup"]);
}

#[test]
fn consumer_failure_stops_later_transaction_completions() {
    let inputs = Inputs::default();
    let mut tags = Vec::new();
    let error = execute_simple_query(
        &inputs.context(),
        "COMMIT; ROLLBACK",
        &[],
        false,
        &mut |result| {
            tags.push(result.command_tag.clone().unwrap());
            Err(SQLError::Internal("consumer disconnected".into()))
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("consumer disconnected"));
    assert_eq!(tags, ["COMMIT"]);
    assert_eq!(inputs.notices.lock().len(), 1);
    inputs.assert_events(&["cache.lookup"]);
}

#[test]
fn cached_statement_relowers_under_the_snapshot_and_retains_guard_through_abort() {
    let statement = Arc::new(uqa_sql::compile("DELETE FROM items").unwrap().remove(0));
    let plan = Arc::new(UnifiedPlan::lower(statement.as_ref().clone()));
    let inputs = Inputs {
        cached: Some((statement, plan)),
        ..Inputs::default()
    };
    inputs.depth.set(1);
    let error = execute(&inputs.context(), "DELETE FROM items", &[]).unwrap_err();
    assert!(error.to_string().contains("injected planning failure"));
    inputs.assert_events(&[
        "cache.lookup",
        "guard.enter",
        "usable",
        "snapshot.true",
        "cache.write.true",
        "optimize.true",
        "abort.true",
        "guard.drop",
    ]);
    assert_eq!(inputs.depth.get(), 1);
    assert!(!inputs.guarded.get());
}

#[test]
fn autocommit_failure_rolls_back_before_releasing_the_statement_guard() {
    let inputs = Inputs::default();
    let error = execute(&inputs.context(), "DELETE FROM items", &[]).unwrap_err();
    assert!(error.to_string().contains("injected planning failure"));
    inputs.assert_events(&[
        "cache.lookup",
        "cache.write.false",
        "guard.enter",
        "begin.statement.false",
        "cache.write.true",
        "optimize.true",
        "rollback.true",
        "guard.drop",
    ]);
    assert_eq!(inputs.depth.get(), 0);
    assert!(!inputs.guarded.get());
}

#[test]
fn implicit_batch_failure_aborts_then_releases_guard_before_segment_rollback() {
    let inputs = Inputs::default();
    let error = execute(
        &inputs.context(),
        "DELETE FROM items; DELETE FROM items",
        &[],
    )
    .unwrap_err();
    assert!(error.to_string().contains("injected planning failure"));
    inputs.assert_events(&[
        "cache.lookup",
        "begin.segment",
        "guard.enter",
        "usable",
        "snapshot.true",
        "optimize.true",
        "abort.true",
        "guard.drop",
        "rollback.false",
    ]);
    assert_eq!(inputs.depth.get(), 0);
    assert!(!inputs.guarded.get());
}
