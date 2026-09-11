//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    abort_explicit_statement_error, rollback_after_statement_error, rollback_implicit_statement,
    validate_transaction_plan, StatementTransactions,
};
use std::cell::RefCell;
use uqa_sql::{plan::UnifiedPlan, SQLError};

struct Transactions {
    depth: usize,
    read_only: bool,
    rollback_fails: bool,
    events: RefCell<Vec<&'static str>>,
}
impl Transactions {
    fn new(depth: usize) -> Self {
        Self {
            depth,
            read_only: false,
            rollback_fails: false,
            events: RefCell::new(Vec::new()),
        }
    }
}
impl StatementTransactions for Transactions {
    fn transaction_depth(&self) -> usize {
        self.events.borrow_mut().push("depth");
        self.depth
    }
    fn current_transaction_is_read_only(&self) -> bool {
        self.events.borrow_mut().push("read-only");
        self.read_only
    }
    fn mark_transaction_snapshot_set(&self) {
        self.events.borrow_mut().push("snapshot");
    }
    fn abort_after_error(&self, error: SQLError) -> SQLError {
        self.events.borrow_mut().push("abort");
        error
    }
    fn rollback(&self) -> Result<(), SQLError> {
        self.events.borrow_mut().push("rollback");
        if self.rollback_fails {
            Err(SQLError::Internal("rollback failure".into()))
        } else {
            Ok(())
        }
    }
}
fn failure() -> SQLError {
    SQLError::Routine {
        sqlstate: "22012".into(),
        message: "statement failure".into(),
    }
}

#[test]
fn explicit_error_abort_requires_an_active_transaction_and_keeps_the_primary_error() {
    for depth in [0, 1] {
        let transactions = Transactions::new(depth);
        let error = abort_explicit_statement_error(&transactions, failure());
        assert!(
            matches!(error, SQLError::Routine { sqlstate, message } if sqlstate == "22012" && message == "statement failure")
        );
        let expected = if depth == 0 {
            vec!["depth"]
        } else {
            vec!["depth", "abort"]
        };
        assert_eq!(*transactions.events.borrow(), expected);
    }
}

#[test]
fn statement_rollback_keeps_both_failures_and_does_not_rollback_a_closed_transaction() {
    let closed = Transactions::new(0);
    assert!(
        matches!(rollback_after_statement_error::<()>(&closed, failure()).unwrap_err(), SQLError::Routine { sqlstate, .. } if sqlstate == "22012")
    );
    assert_eq!(*closed.events.borrow(), ["depth"]);
    let active = Transactions::new(1);
    assert!(
        matches!(rollback_after_statement_error::<()>(&active, failure()).unwrap_err(), SQLError::Routine { sqlstate, .. } if sqlstate == "22012")
    );
    assert_eq!(*active.events.borrow(), ["depth", "rollback"]);
    let broken = Transactions {
        rollback_fails: true,
        ..Transactions::new(1)
    };
    let error = rollback_after_statement_error::<()>(&broken, failure()).unwrap_err();
    assert!(
        matches!(error, SQLError::Internal(message) if message.contains("statement failure") && message.contains("rollback failure"))
    );
    let error = rollback_implicit_statement(&broken, "restart reader").unwrap_err();
    assert!(
        matches!(error, SQLError::Internal(message) if message.starts_with("restart reader: autocommit rollback failed:") && message.contains("rollback failure"))
    );
}

struct Catalog;
impl uqa_sql::FunctionTypeResolver for Catalog {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&uqa_sql::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<uqa_sql::ColumnType>],
        _: bool,
    ) -> Result<Option<uqa_sql::ColumnType>, SQLError> {
        Ok(None)
    }
}
impl uqa_sql::routines::RoutineResolution for Catalog {}
impl uqa_sql::expr::EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("validation must not execute sequences")
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("validation must not execute sequences")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        panic!("validation must not execute sequences")
    }
}
impl uqa_sql::semantics::effects::QueryEffectCatalog for Catalog {
    fn registered_runtime_function_may_mutate_engine(&self, _: &str) -> bool {
        false
    }
    fn domain_by_oid(&self, _: u32) -> Option<uqa_sql::catalog::domain::StoredDomain> {
        None
    }
    fn sequence_persistence(
        &self,
        _: &str,
    ) -> Result<Option<uqa_sql::ast::RelationPersistence>, String> {
        Ok(None)
    }
    fn table_persistence(
        &self,
        _: &str,
    ) -> Result<Option<uqa_sql::ast::RelationPersistence>, String> {
        Err("catalog failure".into())
    }
    fn view_plan(&self, _: &str) -> Result<Option<uqa_sql::plan::QueryPlan>, SQLError> {
        Ok(None)
    }
    fn lookup_prepared(&self, _: &str) -> Option<UnifiedPlan> {
        None
    }
}

#[test]
fn read_only_validation_and_catalog_errors_precede_transaction_snapshot_marking() {
    let effects = uqa_sql::semantics::effects::QueryEffectContext {
        catalog: &Catalog,
        optimizer_effects: |_| false,
        graph_effects: |_| Ok(false),
    };
    let writable = Transactions::new(1);
    let create = UnifiedPlan::lower(
        uqa_sql::compile("CREATE TABLE t (id integer)")
            .unwrap()
            .remove(0),
    );
    validate_transaction_plan(&writable, &effects, &create).unwrap();
    assert_eq!(*writable.events.borrow(), ["read-only", "snapshot"]);
    let reader = Transactions {
        read_only: true,
        ..Transactions::new(1)
    };
    let error = validate_transaction_plan(&reader, &effects, &create).unwrap_err();
    assert!(matches!(error, SQLError::Routine { sqlstate, .. } if sqlstate == "25006"));
    assert_eq!(*reader.events.borrow(), ["read-only"]);
    reader.events.borrow_mut().clear();
    let insert = UnifiedPlan::lower(
        uqa_sql::compile("INSERT INTO t VALUES (1)")
            .unwrap()
            .remove(0),
    );
    assert!(validate_transaction_plan(&reader, &effects, &insert)
        .unwrap_err()
        .to_string()
        .contains("catalog failure"));
    assert_eq!(*reader.events.borrow(), ["read-only"]);
    reader.events.borrow_mut().clear();
    let show = UnifiedPlan::lower(uqa_sql::compile("SHOW work_mem").unwrap().remove(0));
    validate_transaction_plan(&reader, &effects, &show).unwrap();
    assert_eq!(*reader.events.borrow(), ["read-only"]);
}
