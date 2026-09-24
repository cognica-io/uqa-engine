//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{ParameterAssignment::*, ParameterScopes};
use std::collections::BTreeMap;

#[test]
fn configured_local_and_unconfigured_local_have_different_restore_boundaries() {
    let mut parameters = ParameterScopes::default();
    let scope = parameters.enter_function();
    parameters.assigned("application_name".into(), "caller", Save, true);
    parameters.assigned("application_name".into(), "configured", Local, true);
    parameters.assigned("role".into(), "none", Local, true);
    assert_eq!(
        parameters.leave_function(scope),
        BTreeMap::from([("application_name".into(), "caller")])
    );
    assert_eq!(
        parameters.finish_transaction(),
        BTreeMap::from([("role".into(), "none")])
    );
}

#[test]
fn function_configuration_preserves_preceding_transaction_local_restore() {
    let mut parameters = ParameterScopes::default();
    parameters.assigned("role".into(), "original", Local, true);
    let scope = parameters.enter_function();
    parameters.assigned("role".into(), "local", Save, true);
    parameters.assigned("role".into(), "configured", Local, true);
    assert_eq!(
        parameters.leave_function(scope),
        BTreeMap::from([("role".into(), "local")])
    );
    assert_eq!(
        parameters.finish_transaction(),
        BTreeMap::from([("role".into(), "original")])
    );
}

#[test]
fn nested_session_assignment_overrides_ancestors_but_subsequent_local_restores_it() {
    let mut parameters = ParameterScopes::default();
    parameters.assigned("role".into(), "original", Local, true);
    let outer = parameters.enter_function();
    parameters.assigned("role".into(), "transaction local", Save, true);
    let inner = parameters.enter_function();
    parameters.assigned("role".into(), "outer", Save, true);
    parameters.assigned("role".into(), "inner", Session, true);
    parameters.assigned("role".into(), "session", Local, true);
    assert!(parameters.leave_function(inner).is_empty());
    assert!(parameters.leave_function(outer).is_empty());
    assert_eq!(
        parameters.finish_transaction(),
        BTreeMap::from([("role".into(), "session")])
    );
}

#[test]
fn reset_all_keeps_authorization_and_clears_configured_ordinary_settings() {
    let mut parameters = ParameterScopes::default();
    parameters.assigned("role".into(), "original", Local, true);
    let scope = parameters.enter_function();
    parameters.assigned("role".into(), "local", Save, true);
    parameters.assigned("application_name".into(), "caller", Save, true);
    parameters.reset_all();
    assert_eq!(
        parameters.leave_function(scope),
        BTreeMap::from([("role".into(), "local")])
    );
    assert_eq!(
        parameters.finish_transaction(),
        BTreeMap::from([("role".into(), "original")])
    );
}

#[test]
fn local_outside_transaction_restores_immediately_without_retaining_state() {
    let mut parameters = ParameterScopes::default();
    assert_eq!(
        parameters.assigned("role".into(), "original", Local, false),
        Some("original")
    );
    assert!(parameters.finish_transaction().is_empty());
}
