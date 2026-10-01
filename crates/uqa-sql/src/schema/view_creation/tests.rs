//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::ColumnType;
use std::cell::RefCell;

struct Namespace {
    calls: RefCell<Vec<(String, RelationPersistence)>>,
}
impl ViewCreationNamespace for Namespace {
    fn relation_target(
        &self,
        name: &str,
        persistence: RelationPersistence,
    ) -> Result<(String, RelationPersistence), SQLError> {
        self.calls.borrow_mut().push((name.into(), persistence));
        Ok((format!("resolved.{name}"), persistence))
    }
}

#[test]
fn a_view_over_temporary_relations_is_placed_as_a_temporary_relation() {
    let namespace = Namespace {
        calls: RefCell::new(Vec::new()),
    };
    for (name, persistence, uses_temporary_relation, placed) in [
        (
            "private.v",
            RelationPersistence::Permanent,
            true,
            RelationPersistence::Temporary,
        ),
        (
            "v",
            RelationPersistence::Temporary,
            false,
            RelationPersistence::Temporary,
        ),
        (
            "v",
            RelationPersistence::Permanent,
            false,
            RelationPersistence::Permanent,
        ),
    ] {
        namespace.calls.borrow_mut().clear();
        view_creation_target(&namespace, name, persistence, uses_temporary_relation).unwrap();
        assert_eq!(*namespace.calls.borrow(), [(name.to_string(), placed)]);
    }
    assert!(view_becomes_temporary(RelationPersistence::Permanent, true));
    assert!(!view_becomes_temporary(
        RelationPersistence::Temporary,
        true
    ));
    assert!(!view_becomes_temporary(
        RelationPersistence::Permanent,
        false
    ));
    assert_eq!(
        temporary_view_notice("sales.\"Recent\"").unwrap().message,
        "view \"Recent\" will be a temporary view"
    );
}

#[test]
fn view_collision_diagnostics_precede_replacement_kind_checks() {
    for kind in ["view", "table", "materialized view", "composite type"] {
        let error = replacement_is_view("public.v", Some(kind), false).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P07"));
        assert_eq!(error.to_string(), "relation \"v\" already exists");
    }
    for kind in ["table", "composite type"] {
        let error = replacement_is_view("public.v", Some(kind), true).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42809"));
        assert_eq!(error.to_string(), "\"v\" is not a view");
    }
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
