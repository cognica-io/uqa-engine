//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{CreateRule, CreateTrigger, Statement},
    catalog::view::StoredViewKind,
};
mod fixtures;
use fixtures::Catalog;

fn trigger() -> CreateTrigger {
    let Statement::CreateTrigger(definition) = crate::compile(
        "CREATE TRIGGER inspected BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION handler()",
    )
    .unwrap()
    .remove(0) else {
        panic!("expected trigger")
    };
    definition
}
fn rule(sql: &str) -> CreateRule {
    let Statement::CreateRule(definition) = crate::compile(sql).unwrap().remove(0) else {
        panic!("expected rule")
    };
    definition
}
fn error_state(error: SQLError, state: &str, message: &str) {
    let SQLError::Routine {
        sqlstate,
        message: actual,
    } = error
    else {
        panic!("expected routine error")
    };
    assert_eq!(sqlstate, state);
    assert_eq!(actual, message);
}

#[test]
fn constraint_shape_errors_precede_catalog_and_privilege_reads() {
    let catalog = Catalog::default();
    let mut definition = trigger();
    definition.constraint = true;
    definition.or_replace = true;
    error_state(
        catalog
            .context()
            .validate_trigger_definition(&mut definition, RelationLookupMode::Dynamic)
            .unwrap_err(),
        "0A000",
        "CREATE OR REPLACE CONSTRAINT TRIGGER is not supported",
    );
    definition.or_replace = false;
    error_state(
        catalog
            .context()
            .validate_trigger_definition(&mut definition, RelationLookupMode::Dynamic)
            .unwrap_err(),
        "0A000",
        "constraint triggers must be AFTER ROW triggers",
    );
    assert!(catalog.events.lock().unwrap().is_empty());
    assert_eq!(definition.table, "items");
}
#[test]
fn relation_authorization_precedes_trigger_routine_lookup() {
    let catalog = Catalog {
        allow_trigger: false,
        ..Catalog::default()
    };
    let mut definition = trigger();
    error_state(
        catalog
            .context()
            .validate_trigger_definition(&mut definition, RelationLookupMode::Dynamic)
            .unwrap_err(),
        "42501",
        "permission denied for table items",
    );
    assert_eq!(
        *catalog.events.lock().unwrap(),
        [
            "visible:items",
            "current-user",
            "trigger-privilege:public.items:reader"
        ]
    );
    assert_eq!(definition.table, "public.items");
    assert_eq!(definition.function, "handler");
}
#[test]
fn restored_triggers_use_bound_routine_names_without_creation_privileges() {
    let catalog = Catalog {
        allow_owner: false,
        allow_trigger: false,
        ..Catalog::default()
    };
    let mut definition = trigger();
    let (relation, changed) = catalog
        .context()
        .validate_trigger_definition(&mut definition, RelationLookupMode::Bound)
        .unwrap();
    assert_eq!(relation, RelationIdentity::new("public", "items"));
    assert!(!changed);
    assert_eq!(definition.function, "public.handler");
    assert_eq!(
        *catalog.events.lock().unwrap(),
        [
            "bound:items",
            "routine-bound:handler",
            "columns:public.items"
        ]
    );
}
#[test]
fn routine_execute_denial_precedes_trigger_return_type_validation() {
    let mut catalog = Catalog {
        allow_owner: false,
        ..Catalog::default()
    };
    let function = std::sync::Arc::make_mut(&mut catalog.routines[0]);
    function.def.returns = crate::ast::FunctionReturns::Scalar {
        type_name: "integer".into(),
    };
    function.def.execute_acl = Some(Vec::new());
    let mut definition = trigger();
    error_state(
        catalog
            .context()
            .validate_trigger_definition(&mut definition, RelationLookupMode::Dynamic)
            .unwrap_err(),
        "42501",
        "permission denied for function handler",
    );
    assert_eq!(
        *catalog.events.lock().unwrap(),
        [
            "visible:items",
            "current-user",
            "trigger-privilege:public.items:reader",
            "routine-visible:handler",
            "current-user",
            "superuser",
            "inherits:owner"
        ]
    );
    assert_eq!(definition.function, "handler");
}
#[test]
fn stored_trigger_identity_failure_cannot_fall_back_to_a_recreated_name() {
    let catalog = Catalog::default();
    let result = catalog
        .context()
        .resolve_bound_trigger_function("public.handler", Some([17; 16]));
    let Err(error) = result else {
        panic!("old identity must be absent despite a matching live name")
    };
    error_state(error, "42883", "function public.handler() does not exist");
    assert_eq!(
        *catalog.events.lock().unwrap(),
        ["routine-id:public.handler"]
    );
}
#[test]
fn rule_owner_failure_precedes_materialized_view_restrictions() {
    let mut catalog = Catalog {
        kind: "materialized view",
        allow_owner: false,
        ..Catalog::default()
    };
    let mut definition = rule("CREATE RULE inspected AS ON INSERT TO items DO INSTEAD NOTHING");
    error_state(
        catalog
            .context()
            .validate_rule_definition(&mut definition, RelationLookupMode::Dynamic, None, None)
            .unwrap_err(),
        "42501",
        "must be owner of materialized view items",
    );
    assert_eq!(
        *catalog.events.lock().unwrap(),
        ["visible:items", "owner:public.items", "inherits:owner"]
    );
    catalog.events.lock().unwrap().clear();
    catalog.allow_owner = true;
    error_state(
        catalog
            .context()
            .validate_rule_definition(&mut definition, RelationLookupMode::Dynamic, None, None)
            .unwrap_err(),
        "0A000",
        "rules on materialized views are not supported",
    );
    assert_eq!(
        *catalog.events.lock().unwrap(),
        [
            "visible:public.items",
            "owner:public.items",
            "inherits:owner",
            "view-kind"
        ]
    );
}
#[test]
fn invalid_view_return_rules_fail_before_row_metadata_and_action_analysis() {
    let catalog = Catalog {
        kind: "view",
        ..Catalog::default()
    };
    let mut definition =
        rule("CREATE RULE inspected AS ON SELECT TO items DO INSTEAD SELECT missing");
    error_state(
        catalog
            .context()
            .validate_rule_definition(&mut definition, RelationLookupMode::Bound, None, None)
            .unwrap_err(),
        "42P17",
        "view rule must be named \"_RETURN\", unconditional, INSTEAD, and have one SELECT action",
    );
    assert_eq!(
        *catalog.events.lock().unwrap(),
        ["bound:items", "view-kind"]
    );
}

#[test]
fn restoration_requires_the_current_bound_function_object_identity() {
    let mut catalog = Catalog::default();
    let definition = trigger();
    let error = crate::catalog::events::restoration::trigger_function_object_id(
        &catalog.context(),
        &definition,
    )
    .unwrap_err();
    assert_eq!(
        error,
        "restore trigger catalog: function `handler` has no object identity"
    );
    std::sync::Arc::make_mut(&mut catalog.routines[0])
        .def
        .object_id = Some([33; 16]);
    assert_eq!(
        crate::catalog::events::restoration::trigger_function_object_id(
            &catalog.context(),
            &definition
        )
        .unwrap(),
        [33; 16]
    );
    assert_eq!(
        *catalog.events.lock().unwrap(),
        ["routine-bound:handler", "routine-bound:handler"]
    );
}

mod partitions;

mod selections;
