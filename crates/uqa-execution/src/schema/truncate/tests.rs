//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
};

#[derive(Default)]
struct Inputs {
    events: RefCell<Vec<String>>,
    depth: Cell<usize>,
    references: RefCell<BTreeMap<String, Vec<String>>>,
    fail: Option<&'static str>,
    change_references_before: bool,
}
impl Inputs {
    fn context(&self) -> TruncateContext<'_> {
        TruncateContext {
            catalog: self,
            access: self,
            triggers: self,
            storage: self,
            transactions: self,
        }
    }
    fn event(&self, event: String) {
        self.events.borrow_mut().push(event);
    }
    fn failure(&self, at: &str) -> Result<(), SQLError> {
        if self.fail == Some(at) {
            Err(SQLError::Internal(format!("injected {at}")))
        } else {
            Ok(())
        }
    }
}
impl TruncateCatalog for Inputs {
    fn try_resolve_visible_relation_kind(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        self.event(format!("resolve:{name}"));
        Ok(Some((name.into(), "table")))
    }
    fn is_partitioned(&self, table: &str) -> Result<bool, String> {
        self.event(format!("partition:{table}"));
        Ok(false)
    }
    fn hierarchy_scan_tables(&self, table: &str, _: bool) -> Result<Vec<String>, SQLError> {
        self.event(format!("hierarchy:{table}"));
        Ok(vec![table.into()])
    }
    fn referrers_to(&self, table: &str) -> Result<Vec<String>, String> {
        self.event(format!("references:{table}"));
        Ok(self
            .references
            .borrow()
            .get(table)
            .cloned()
            .unwrap_or_default())
    }
}
impl TruncateAccess for Inputs {
    fn ensure_truncate_privilege(&self, table: &str) -> Result<(), SQLError> {
        self.event(format!("privilege:{table}"));
        self.failure("privilege")
    }
}
impl TruncateTriggers for Inputs {
    fn ensure_no_pending_trigger_events(
        &self,
        table: &str,
        operation: &str,
    ) -> Result<(), SQLError> {
        assert_eq!(operation, "TRUNCATE");
        self.event(format!("pending:{table}"));
        self.failure("pending")
    }
    fn fire_statement_trigger(&self, table: &str, timing: TriggerTiming) -> Result<(), SQLError> {
        if timing == TriggerTiming::Before {
            self.event(format!("before:{table}"));
            self.failure("before")?;
            if self.change_references_before && table == "a" {
                self.references
                    .borrow_mut()
                    .insert("a".into(), vec!["b".into()]);
            }
        } else {
            assert_eq!(timing, TriggerTiming::After);
            self.event(format!("after:{table}"));
        }
        Ok(())
    }
}
impl TruncateStorage for Inputs {
    fn truncate_tables_with_identity(
        &self,
        tables: &[String],
        restart_identity: bool,
    ) -> Result<(), SQLError> {
        assert_eq!(self.depth.get(), 1);
        self.event(format!("clear:{}:{restart_identity}", tables.join(",")));
        self.failure("clear")
    }
}
impl TruncateTransactions for Inputs {
    fn transaction_depth(&self) -> usize {
        self.event("depth".into());
        self.depth.get()
    }
    fn with_transaction(&self, operation: TruncateWrite<'_>) -> Result<(), SQLError> {
        self.event("begin".into());
        self.depth.set(1);
        let result = operation(&self.context());
        self.event(if result.is_ok() { "commit" } else { "rollback" }.into());
        self.depth.set(0);
        result
    }
}
fn target(table: &str) -> TruncateTarget {
    TruncateTarget {
        table: table.into(),
        include_descendants: true,
    }
}

#[test]
fn privilege_and_pending_trigger_errors_precede_restrict_reads_and_transaction_entry() {
    for fail in ["privilege", "pending"] {
        let inputs = Inputs {
            fail: Some(fail),
            ..Inputs::default()
        };
        let error = execute(&inputs.context(), &[target("a")], false, false).unwrap_err();
        assert!(
            matches!(error, SQLError::Internal(message) if message == format!("injected {fail}"))
        );
        let events = inputs.events.borrow();
        assert!(!events
            .iter()
            .any(|event| event == "references:a" || event == "depth" || event == "begin"));
        assert_eq!(events.last().unwrap(), &format!("{fail}:a"));
    }
}

#[test]
fn all_before_triggers_precede_fresh_dependency_order_and_after_triggers_keep_statement_order() {
    let inputs = Inputs {
        change_references_before: true,
        ..Inputs::default()
    };
    execute(&inputs.context(), &[target("a"), target("b")], false, false).unwrap();
    let events = inputs.events.borrow();
    let begin = events.iter().position(|event| event == "begin").unwrap();
    assert_eq!(
        &events[begin..],
        [
            "begin",
            "before:a",
            "before:b",
            "references:a",
            "references:b",
            "clear:b,a:false",
            "after:a",
            "after:b",
            "commit"
        ]
    );
    assert_eq!(inputs.depth.get(), 0);
}

#[test]
fn clear_failure_rolls_back_owned_frame_and_skips_after_triggers() {
    let inputs = Inputs {
        fail: Some("clear"),
        ..Inputs::default()
    };
    let error = execute(&inputs.context(), &[target("a")], false, true).unwrap_err();
    assert!(matches!(error, SQLError::Internal(message) if message == "injected clear"));
    let events = inputs.events.borrow();
    assert!(events.iter().any(|event| event == "clear:a:true"));
    assert!(!events.iter().any(|event| event.starts_with("after:")));
    assert_eq!(events.last().unwrap(), "rollback");
    assert_eq!(inputs.depth.get(), 0);
}

#[test]
fn an_existing_transaction_is_retained_on_success_and_before_trigger_error() {
    for fail in [None, Some("before")] {
        let inputs = Inputs {
            depth: Cell::new(1),
            fail,
            ..Inputs::default()
        };
        let result = execute(&inputs.context(), &[target("a")], false, false);
        assert_eq!(result.is_err(), fail.is_some());
        assert_eq!(inputs.depth.get(), 1);
        assert!(!inputs
            .events
            .borrow()
            .iter()
            .any(|event| ["begin", "commit", "rollback"].contains(&event.as_str())));
    }
}
