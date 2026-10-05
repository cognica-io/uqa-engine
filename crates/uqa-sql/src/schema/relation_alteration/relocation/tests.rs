//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::resolution::RelationResolution;
use std::cell::Cell;

#[derive(Default)]
struct Names {
    occupied: bool,
    reads: Cell<usize>,
}

impl RelationAlterNames for Names {
    fn resolve_relation_kind(&self, _: &str) -> Result<RelationResolution, SQLError> {
        unreachable!("a schema move already holds its source identity")
    }

    fn relation_kind_at(&self, _: &str) -> Result<Option<&'static str>, String> {
        self.reads.set(self.reads.get() + 1);
        Ok(self.occupied.then_some("table"))
    }
}

#[test]
fn unchanged_schema_is_a_no_op_but_temporary_namespaces_are_rejected_first() {
    let names = Names {
        occupied: true,
        ..Names::default()
    };
    let source = RelationIdentity::new("app", "items");
    assert!(!validate_target(
        &names,
        &source,
        &source,
        RelationPersistence::Permanent,
        "pg_temp_1"
    )
    .unwrap());
    for (source, target, persistence) in [
        (
            source.clone(),
            source.clone(),
            RelationPersistence::Temporary,
        ),
        (
            RelationIdentity::new("pg_temp_1", "items"),
            RelationIdentity::new("pg_temp_1", "items"),
            RelationPersistence::Permanent,
        ),
        (
            source,
            RelationIdentity::new("pg_temp_2", "items"),
            RelationPersistence::Permanent,
        ),
    ] {
        let error =
            validate_target(&names, &source, &target, persistence, "pg_temp_1").unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"));
        assert!(error
            .to_string()
            .contains("cannot move objects into or out of temporary schemas"));
    }
    assert_eq!(names.reads.get(), 0);
}

#[test]
fn destination_collision_preserves_the_schema_and_local_name() {
    let source = RelationIdentity::new("app", "Items");
    let target = declared_target(&source, "\"New.Schema\"").unwrap();
    assert_eq!(target, RelationIdentity::new("New.Schema", "Items"));
    let names = Names {
        occupied: true,
        ..Names::default()
    };
    let error = validate_target(
        &names,
        &source,
        &target,
        RelationPersistence::Permanent,
        "pg_temp_1",
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P07"));
    assert!(error
        .to_string()
        .contains("relation \"Items\" already exists in schema \"New.Schema\""));
    assert_eq!(names.reads.get(), 1);
}

#[test]
fn namespace_move_with_no_collision_uses_the_existing_relation_identity() {
    let names = Names::default();
    assert!(validate_target(
        &names,
        &RelationIdentity::new("app", "items"),
        &RelationIdentity::new("archive", "items"),
        RelationPersistence::Unlogged,
        "pg_temp_1"
    )
    .unwrap());
    assert_eq!(names.reads.get(), 1);
}
