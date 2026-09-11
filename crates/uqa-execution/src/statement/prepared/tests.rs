//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::{Cell, RefCell};
use uqa_sql::binding::statements::{
    StatementAnalysisOperation, StatementAnalysisScopes, StatementBindingScope,
};
mod fixtures;
use fixtures::NoRoutines;

thread_local! {
    static EVENTS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    static LOCKED: Cell<bool> = const { Cell::new(false) };
}
fn record(event: &'static str) {
    EVENTS.with(|events| events.borrow_mut().push(event));
}
fn clock() -> i64 {
    assert!(LOCKED.with(Cell::get));
    record("clock");
    42
}
#[derive(Default)]
struct Registry {
    fail: bool,
    entry: RefCell<Option<PreparedStatementPlan>>,
}
struct Scope<'a>(&'a Registry);
impl Drop for Scope<'_> {
    fn drop(&mut self) {
        record("release");
    }
}
impl StatementBindingScope for Scope<'_> {
    fn binding_context(&self) -> Result<uqa_sql::binding::context::BindingContext<'_>, SQLError> {
        record("binding");
        if self.0.fail {
            return Err(SQLError::Internal("analysis stopped".into()));
        }
        Ok(fixtures::binding_context())
    }
}
impl StatementAnalysisScopes for Registry {
    fn with_scope(&self, analyze: StatementAnalysisOperation<'_>) -> Result<(), SQLError> {
        record("scope");
        let scope = Scope(self);
        analyze(&scope)
    }
}
impl AggregateClassifier for Registry {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}
struct Write<'a>(&'a Registry);
impl Drop for Write<'_> {
    fn drop(&mut self) {
        assert!(LOCKED.with(|locked| locked.replace(false)));
        record("unlock");
    }
}
impl PreparedDefinitionWrite for Write<'_> {
    fn insert(&mut self, name: String, definition: PreparedStatementPlan) {
        assert!(LOCKED.with(Cell::get));
        assert_eq!(name, "saved");
        record("insert");
        *self.0.entry.borrow_mut() = Some(definition);
    }
}
impl PreparedDefinitionRegistry for Registry {
    fn write_definitions(&self) -> Box<dyn PreparedDefinitionWrite + '_> {
        assert!(!LOCKED.with(|locked| locked.replace(true)));
        record("write");
        Box::new(Write(self))
    }
}
fn register(registry: &Registry) -> Result<(), SQLError> {
    EVENTS.with(|events| events.borrow_mut().clear());
    let query = "SELECT $1::integer AS value";
    register_plan(
        &PreparedRegistrationContext {
            analysis: PreparedDefinitionContext {
                types: &NoRoutines,
                routines: &NoRoutines,
                scopes: registry,
            },
            aggregates: registry,
            registry,
            clock,
        },
        "saved".into(),
        UnifiedPlan::lower(uqa_sql::compile(query).unwrap().remove(0)),
        &[],
        Some(query),
    )
}

#[test]
fn registration_samples_clock_and_publishes_under_the_retained_write_guard() {
    let registry = Registry::default();
    register(&registry).unwrap();
    EVENTS.with(|events| {
        assert_eq!(
            *events.borrow(),
            [
                "scope", "binding", "release", "scope", "binding", "release", "write", "clock",
                "insert", "unlock"
            ]
        );
    });
    let entry = registry.entry.borrow();
    let entry = entry.as_ref().unwrap();
    assert_eq!(entry.prepared_at_micros, 42);
    assert!(entry.from_sql);
    assert_eq!(
        entry.source_sql.as_deref(),
        Some("SELECT $1::integer AS value")
    );
    assert_eq!(entry.parameter_types, [Some(ColumnType::Integer)]);
    assert!(entry.plan.is_none());
    assert_eq!(entry.generic_plans, 0);
    assert_eq!(entry.custom_plans, 0);
}

#[test]
fn failed_analysis_never_locks_the_registry_or_samples_its_clock() {
    let registry = Registry {
        fail: true,
        ..Registry::default()
    };
    assert!(
        matches!(register(&registry), Err(SQLError::Internal(message)) if message == "analysis stopped")
    );
    EVENTS.with(|events| assert_eq!(*events.borrow(), ["scope", "binding", "release"]));
    assert!(registry.entry.borrow().is_none());
    assert!(!LOCKED.with(Cell::get));
}
