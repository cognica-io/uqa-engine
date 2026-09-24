//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` DROP target diagnostics and canonical target order.

use super::*;

struct Catalog;

impl RelationDropCatalog for Catalog {
    fn resolve_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError> {
        Ok(match name {
            "absent.item" => RelationResolution::MissingSchema("absent".into()),
            "missing" | "public.missing" => RelationResolution::MissingRelation,
            "z" | "public.z" => RelationResolution::Found("public.z".into(), "view"),
            "a" => RelationResolution::Found("public.a".into(), "view"),
            _ => RelationResolution::Found("public.t".into(), "table"),
        })
    }
    fn resolve_age_label_relation_name(&self, _: &str) -> Result<Option<String>, SQLError> {
        Ok(None)
    }
}

#[test]
fn drop_missing_targets_use_kind_specific_states_and_schema_notices() {
    for (kind, state, label) in [
        (DropKind::Table, "42P01", "table"),
        (DropKind::View, "42P01", "view"),
        (DropKind::MaterializedView, "42P01", "materialized view"),
        (DropKind::Sequence, "42P01", "sequence"),
        (DropKind::ForeignTable, "42704", "foreign table"),
    ] {
        for (name, expected, message) in [
            (
                "public.missing",
                state,
                format!("{label} \"missing\" does not exist"),
            ),
            (
                "absent.item",
                "3F000",
                "schema \"absent\" does not exist".into(),
            ),
        ] {
            let error =
                bind_relation_drop_target(&Catalog, name, kind, false, &mut |_| {}).unwrap_err();
            assert_eq!(error.sqlstate(), Some(expected));
            assert_eq!(error.to_string(), message);
            let mut notices = Vec::new();
            assert!(
                bind_relation_drop_target(&Catalog, name, kind, true, &mut |notice| notices
                    .push(notice.to_string()))
                .unwrap()
                .is_none()
            );
            assert_eq!(notices, [format!("{message}, skipping")]);
        }
    }
}

#[test]
fn drop_wrong_kind_is_an_error_even_with_if_exists() {
    for kind in [
        DropKind::View,
        DropKind::MaterializedView,
        DropKind::Sequence,
        DropKind::ForeignTable,
    ] {
        let error = bind_relation_drop_target(&Catalog, "public.t", kind, true, &mut |_| {
            panic!("wrong kind is not a notice")
        })
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42809"));
        assert_eq!(
            error.to_string(),
            format!("\"t\" is not a {}", drop_relation_kind(kind))
        );
    }
}

#[test]
fn drop_deduplicates_canonical_targets_without_sorting_the_statement() {
    let stmt = DropStmt {
        kind: DropKind::View,
        names: vec!["z".into(), "a".into(), "public.z".into()],
        if_exists: false,
        cascade: false,
    };
    assert_eq!(
        bind_relation_drop_targets(&Catalog, &stmt, &mut |_| {}).unwrap(),
        ["public.z", "public.a"]
    );
}
