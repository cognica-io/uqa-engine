//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::binding::statements::StatementAnalysisOperation;
use std::cell::{Cell, RefCell};
mod fixtures;
use fixtures::NoRoutines;

#[derive(Default)]
struct Scopes {
    captures: Cell<usize>,
    fail_at: Option<usize>,
    events: RefCell<Vec<String>>,
}
struct Scope<'a> {
    owner: &'a Scopes,
    index: usize,
}
impl Drop for Scope<'_> {
    fn drop(&mut self) {
        self.owner
            .events
            .borrow_mut()
            .push(format!("{}.release", self.index));
    }
}
impl StatementBindingScope for Scope<'_> {
    fn binding_context(&self) -> Result<crate::binding::context::BindingContext<'_>, SQLError> {
        self.owner
            .events
            .borrow_mut()
            .push(format!("{}.binding", self.index));
        if self.owner.fail_at == Some(self.index) {
            return Err(SQLError::Internal("binding unavailable".into()));
        }
        Ok(fixtures::binding_context())
    }
}
impl StatementAnalysisScopes for Scopes {
    fn with_scope(&self, analyze: StatementAnalysisOperation<'_>) -> Result<(), SQLError> {
        let index = self.captures.get() + 1;
        self.captures.set(index);
        self.events.borrow_mut().push(format!("{index}.capture"));
        let scope = Scope { owner: self, index };
        analyze(&scope)
    }
}
fn analyze(scopes: &Scopes, declared: &[ColumnType]) -> Result<PreparedDefinition, SQLError> {
    analyze_definition(
        &PreparedDefinitionContext {
            types: &NoRoutines,
            routines: &NoRoutines,
            scopes,
        },
        UnifiedPlan::lower(
            crate::compile("SELECT $1::integer AS value")
                .unwrap()
                .remove(0),
        ),
        declared,
    )
}

#[test]
fn inference_scope_is_released_before_result_descriptor_scope() {
    let scopes = Scopes::default();
    let definition = analyze(&scopes, &[]).unwrap();
    assert_eq!(definition.parameter_types, vec![Some(ColumnType::Integer)]);
    assert_eq!(definition.result_schema.unwrap().columns(), &["value"]);
    assert_eq!(
        *scopes.events.borrow(),
        [
            "1.capture",
            "1.binding",
            "1.release",
            "2.capture",
            "2.binding",
            "2.release"
        ]
    );
}

#[test]
fn invalid_declared_type_fails_before_scope_capture() {
    let scopes = Scopes::default();
    assert!(analyze(&scopes, &[ColumnType::Named("public.missing_type".into())]).is_err());
    assert!(scopes.events.borrow().is_empty());
}

#[test]
fn failed_inference_does_not_capture_a_result_descriptor_scope() {
    let scopes = Scopes {
        fail_at: Some(1),
        ..Scopes::default()
    };
    assert!(
        matches!(analyze(&scopes, &[]), Err(SQLError::Internal(message)) if message == "binding unavailable")
    );
    assert_eq!(
        *scopes.events.borrow(),
        ["1.capture", "1.binding", "1.release"]
    );
}

#[test]
fn failed_descriptor_analysis_releases_its_scope() {
    let scopes = Scopes {
        fail_at: Some(2),
        ..Scopes::default()
    };
    assert!(
        matches!(analyze(&scopes, &[]), Err(SQLError::Internal(message)) if message == "binding unavailable")
    );
    assert_eq!(
        *scopes.events.borrow(),
        [
            "1.capture",
            "1.binding",
            "1.release",
            "2.capture",
            "2.binding",
            "2.release"
        ]
    );
}
