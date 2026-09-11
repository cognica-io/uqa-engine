//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::ColumnType;
use std::cell::RefCell;

struct Namespace {
    calls: RefCell<Vec<&'static str>>,
    deny_persistent: bool,
}
impl ViewCreationNamespace for Namespace {
    fn temporary_schema_name(&self) -> String {
        self.calls.borrow_mut().push("temporary_schema");
        "pg_temp_7".into()
    }
    fn temporary_target(&self, _: &str) -> Result<String, SQLError> {
        self.calls.borrow_mut().push("temporary");
        Ok("pg_temp_7.v".into())
    }
    fn persistent_target(&self, _: &str) -> Result<String, SQLError> {
        self.calls.borrow_mut().push("persistent");
        if self.deny_persistent {
            Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "schema creation denied".into(),
            })
        } else {
            Ok("public.v".into())
        }
    }
}

#[test]
fn temporary_view_dependencies_preserve_namespace_privilege_order() {
    let namespace = Namespace {
        calls: RefCell::new(Vec::new()),
        deny_persistent: true,
    };
    let error = view_creation_target(
        &namespace,
        "private.v",
        RelationPersistence::Permanent,
        true,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert_eq!(
        *namespace.calls.borrow(),
        ["temporary_schema", "persistent"]
    );
    namespace.calls.borrow_mut().clear();
    assert_eq!(
        view_creation_target(
            &namespace,
            "pg_temp.v",
            RelationPersistence::Permanent,
            true
        )
        .unwrap(),
        ("pg_temp_7.v".into(), RelationPersistence::Temporary)
    );
    assert_eq!(*namespace.calls.borrow(), ["temporary"]);
    namespace.calls.borrow_mut().clear();
    view_creation_target(&namespace, "v", RelationPersistence::Temporary, false).unwrap();
    assert_eq!(*namespace.calls.borrow(), ["temporary"]);
}

#[test]
fn view_collision_diagnostics_precede_replacement_kind_checks() {
    for kind in ["view", "table", "materialized view"] {
        let error = replacement_is_view("public.v", Some(kind), false).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P07"));
        assert!(error.to_string().contains("already exists"));
    }
    let error = replacement_is_view("public.v", Some("table"), true).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42809"));
    assert!(error.to_string().contains("it is a table"));
    assert!(replacement_is_view("public.v", Some("view"), true).unwrap());
    assert!(!replacement_is_view("public.v", None, false).unwrap());
}

#[test]
fn replacement_row_types_allow_append_but_preserve_existing_names_and_types() {
    let schema = |names: &[&str], types: Vec<Option<ColumnType>>| {
        RowSchema::with_types(names.iter().map(|name| (*name).into()).collect(), types)
    };
    let old = schema(
        &["first", "second"],
        vec![Some(ColumnType::Integer), Some(ColumnType::Text)],
    );
    let appended = schema(
        &["first", "second", "extra"],
        vec![
            Some(ColumnType::Integer),
            Some(ColumnType::Text),
            Some(ColumnType::Boolean),
        ],
    );
    validate_replacement_schema(&old, &appended).unwrap();
    for (new, message) in [
        (
            schema(&["renamed"], vec![Some(ColumnType::Text)]),
            "cannot drop columns from view",
        ),
        (
            schema(
                &["renamed", "second"],
                vec![Some(ColumnType::Text), Some(ColumnType::Text)],
            ),
            "cannot change name of view column",
        ),
        (
            schema(
                &["first", "second"],
                vec![Some(ColumnType::Text), Some(ColumnType::Text)],
            ),
            "cannot change data type of view column",
        ),
    ] {
        let error = validate_replacement_schema(&old, &new).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P16"));
        assert!(error.to_string().contains(message), "{error}");
    }
}
