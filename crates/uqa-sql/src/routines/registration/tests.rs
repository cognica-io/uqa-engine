//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::ColumnType, catalog::roles::RoleIdentity, Statement};

fn definition(sql: &str, defaults: &[Option<ColumnType>]) -> CreateFunction {
    let Statement::CreateFunction(mut definition) = crate::compile(sql).unwrap().remove(0) else {
        panic!("routine definition fixture");
    };
    assert_eq!(
        definition
            .params
            .iter()
            .filter(|parameter| parameter.default.is_some())
            .count(),
        defaults.len()
    );
    let parameters = definition
        .params
        .iter_mut()
        .filter(|parameter| parameter.default.is_some());
    for (parameter, ty) in parameters.zip(defaults) {
        parameter.default_type = ty.clone().map(crate::ast::RoutineDefaultType::Concrete);
    }
    definition.object_id = Some([1; 16]);
    definition.catalog_oid = Some(16_384);
    definition.owner = Some(RoleIdentity::BOOTSTRAP);
    *definition
}

fn replace(
    existing: &CreateFunction,
    replacement: &mut CreateFunction,
    signature: &str,
) -> Result<(), SQLError> {
    replacement.or_replace = true;
    prepare_routine_replacement(
        existing,
        replacement,
        "uqa",
        &BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]),
        &BTreeMap::new(),
        signature,
    )
}

fn assert_diagnostic(error: SQLError, message: &str, hint: &str) {
    assert_eq!(error.sqlstate(), Some("42P13"));
    assert_eq!(error.to_string(), message);
    assert_eq!(error.detail(), None);
    assert_eq!(error.hint(), Some(hint));
}

#[test]
fn replacement_cannot_remove_defaults_from_functions_or_procedures() {
    for (sql, signature, command) in [
        (
            "CREATE FUNCTION f(a integer DEFAULT 1) RETURNS integer LANGUAGE SQL AS 'SELECT a'",
            "f(integer)",
            "FUNCTION",
        ),
        (
            "CREATE PROCEDURE p(a integer DEFAULT 1) LANGUAGE SQL AS 'SELECT a'",
            "p(integer)",
            "PROCEDURE",
        ),
    ] {
        let existing = definition(sql, &[Some(ColumnType::Integer)]);
        let mut replacement = existing.clone();
        replacement.params[0].default = None;
        replacement.params[0].default_type = None;
        let error = replace(&existing, &mut replacement, signature).unwrap_err();
        assert_diagnostic(
            error,
            "cannot remove parameter defaults from existing function",
            &format!("Use DROP {command} {signature} first."),
        );
    }
}

#[test]
fn default_removal_precedes_the_remaining_defaults_type_change() {
    let existing = definition(
        "CREATE FUNCTION f(a integer DEFAULT 1,b anyelement DEFAULT 2) RETURNS anyelement LANGUAGE SQL AS 'SELECT b'",
        &[Some(ColumnType::Integer), Some(ColumnType::Integer)],
    );
    let mut replacement = existing.clone();
    replacement.params[0].default = None;
    replacement.params[0].default_type = None;
    replacement.params[1].default_type = Some(crate::ast::RoutineDefaultType::Concrete(
        ColumnType::BigInteger,
    ));
    assert_diagnostic(
        replace(&existing, &mut replacement, "f(integer,anyelement)").unwrap_err(),
        "cannot remove parameter defaults from existing function",
        "Use DROP FUNCTION f(integer,anyelement) first.",
    );
}

#[test]
fn replacement_compares_the_existing_default_suffix_after_adding_defaults() {
    let existing = definition(
        "CREATE FUNCTION f(a integer,b anyelement DEFAULT 'old'::text) RETURNS anyelement LANGUAGE SQL AS 'SELECT b'",
        &[Some(ColumnType::Text)],
    );
    let mut replacement = definition(
        "CREATE FUNCTION f(a integer DEFAULT 1,b anyelement DEFAULT 'new'::text) RETURNS anyelement LANGUAGE SQL AS 'SELECT b'",
        &[Some(ColumnType::Integer), Some(ColumnType::Text)],
    );
    replacement.object_id = Some([2; 16]);
    replacement.catalog_oid = Some(16_385);
    replace(&existing, &mut replacement, "f(integer,anyelement)").unwrap();
    assert_eq!(replacement.object_id, existing.object_id);
    assert_eq!(replacement.catalog_oid, existing.catalog_oid);
    assert_eq!(replacement.owner, existing.owner);
    replacement.params[1].default_type = Some(crate::ast::RoutineDefaultType::Concrete(
        ColumnType::Varchar(None),
    ));
    assert_diagnostic(
        replace(&existing, &mut replacement, "f(integer,anyelement)").unwrap_err(),
        "cannot change data type of existing parameter default value",
        "Use DROP FUNCTION f(integer,anyelement) first.",
    );
}

#[test]
fn default_type_comparison_uses_type_identity_and_ignores_modifiers() {
    let existing = definition(
        "CREATE FUNCTION f(a anyelement DEFAULT 'x'::varchar(3)) RETURNS anyelement LANGUAGE SQL AS 'SELECT a'",
        &[Some(ColumnType::Varchar(Some(3)))],
    );
    let mut replacement = existing.clone();
    replacement.params[0].default_type = Some(crate::ast::RoutineDefaultType::Concrete(
        ColumnType::Varchar(Some(5)),
    ));
    replace(&existing, &mut replacement, "f(anyelement)").unwrap();
    replacement.params[0].default_type =
        Some(crate::ast::RoutineDefaultType::Concrete(ColumnType::Text));
    assert_diagnostic(
        replace(&existing, &mut replacement, "other.\"Mixed F\"(anyelement)").unwrap_err(),
        "cannot change data type of existing parameter default value",
        "Use DROP FUNCTION other.\"Mixed F\"(anyelement) first.",
    );
}

#[test]
fn replacing_an_unknown_default_with_a_concrete_type_is_rejected() {
    let existing = definition(
        "CREATE FUNCTION f(a anyelement DEFAULT 'x') RETURNS anyelement LANGUAGE SQL AS 'SELECT a'",
        &[None],
    );
    let mut replacement = existing.clone();
    replace(&existing, &mut replacement, "f(anyelement)").unwrap();
    replacement.params[0].default_type =
        Some(crate::ast::RoutineDefaultType::Concrete(ColumnType::Text));
    assert_diagnostic(
        replace(&existing, &mut replacement, "f(anyelement)").unwrap_err(),
        "cannot change data type of existing parameter default value",
        "Use DROP FUNCTION f(anyelement) first.",
    );
}

#[test]
fn concrete_defaults_compare_the_assignment_type_and_preserve_publication_identity() {
    let existing = definition(
        "CREATE FUNCTION f(a integer DEFAULT 1) RETURNS integer LANGUAGE SQL AS 'SELECT a'",
        &[Some(ColumnType::Integer)],
    );
    let mut replacement = definition(
        "CREATE FUNCTION f(a integer DEFAULT 2::bigint) RETURNS integer LANGUAGE SQL AS 'SELECT a'",
        &[Some(ColumnType::Integer)],
    );
    replacement.object_id = Some([2; 16]);
    replace(&existing, &mut replacement, "f(integer)").unwrap();
    assert_eq!(replacement.object_id, existing.object_id);
    assert_eq!(replacement.catalog_oid, existing.catalog_oid);
    assert_eq!(replacement.owner, existing.owner);
    assert_eq!(replacement.execute_acl, existing.execute_acl);
}
