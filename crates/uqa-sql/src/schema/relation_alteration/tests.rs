//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn view_targets_keep_the_actual_kind_until_authority_has_been_checked() {
    for (sql, expected) in [
        ("ALTER VIEW s.target RENAME TO renamed", "view"),
        ("ALTER VIEW s.target SET (security_barrier=true)", "view"),
        (
            "ALTER MATERIALIZED VIEW s.target OWNER TO reader",
            "materialized view",
        ),
    ] {
        let crate::Statement::AlterView(statement) = crate::compile(sql).unwrap().remove(0) else {
            panic!("expected ALTER VIEW: {sql}");
        };
        let target = view_alter_target(
            RelationResolution::Found("s.target".into(), "table"),
            &statement,
            &mut |_| panic!("an existing relation emits no missing-target notice"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(target.canonical, "s.target");
        assert_eq!(target.kind, "table");
        target.require_kind("table").unwrap();
        let error = target.require_kind(expected).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42809"));
        assert!(error
            .to_string()
            .contains(&format!("\"target\" is not a {expected}")));
    }
}

#[test]
fn foreign_targets_keep_the_actual_kind_and_report_the_local_name() {
    let crate::Statement::AlterForeignTable(statement) =
        crate::compile("ALTER FOREIGN TABLE s.target OWNER TO reader")
            .unwrap()
            .remove(0)
    else {
        panic!("expected ALTER FOREIGN TABLE");
    };
    let target = foreign_table_alter_target(
        RelationResolution::Found("s.target".into(), "view"),
        &statement,
        &mut |_| panic!("an existing relation emits no missing-target notice"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(target.kind, "view");
    let error = target.require_kind("foreign table").unwrap_err();
    assert_eq!(error.sqlstate(), Some("42809"));
    assert!(error
        .to_string()
        .contains("\"target\" is not a foreign table"));
}

#[test]
fn materialized_storage_options_use_weaker_locks_than_visible_definition_changes() {
    for (sql, mode) in [
        (
            "ALTER MATERIALIZED VIEW m SET (fillfactor=80)",
            TableLockMode::ShareUpdateExclusive,
        ),
        (
            "ALTER MATERIALIZED VIEW m RESET (fillfactor)",
            TableLockMode::ShareUpdateExclusive,
        ),
        (
            "ALTER MATERIALIZED VIEW m OWNER TO other",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER MATERIALIZED VIEW m RENAME TO other",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER VIEW v SET (security_barrier=true)",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER VIEW v RESET (security_invoker)",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER VIEW v OWNER TO other",
            TableLockMode::AccessExclusive,
        ),
        (
            "ALTER VIEW v RENAME TO other",
            TableLockMode::AccessExclusive,
        ),
    ] {
        let crate::Statement::AlterView(statement) = crate::compile(sql).unwrap().remove(0) else {
            panic!("expected ALTER VIEW: {sql}");
        };
        assert_eq!(view_alter_lock_mode(&statement), mode, "{sql}");
    }
}
