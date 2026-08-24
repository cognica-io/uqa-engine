//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER, CTAS, SELECT INTO, lifecycle, and analysis-order compilation.

use super::*;

#[test]
fn alter_table_add_key_constraint_preserves_tuple_shape() {
    let Statement::AlterTable(alter) =
        first("ALTER TABLE labels ADD CONSTRAINT labels_tenant_slug_key UNIQUE (tenant, slug)")
    else {
        panic!("expected ALTER TABLE");
    };
    assert!(matches!(
        alter.action,
        AlterTableAction::AddKeyConstraint { constraint }
            if constraint.name.as_deref() == Some("labels_tenant_slug_key")
                && constraint.kind == TableKeyConstraintKind::Unique
                && constraint.columns == ["tenant", "slug"]
    ));
}

#[test]
fn alter_sequence_preserves_if_exists() {
    let Statement::AlterSequence(sequence) =
        first("ALTER SEQUENCE IF EXISTS absent RESTART WITH 7")
    else {
        panic!("expected ALTER SEQUENCE");
    };
    assert!(sequence.if_exists);
    assert_eq!(sequence.restart, crate::ast::SequenceRestart::With(7));
}

#[test]
fn create_table_as_preserves_positional_column_names() {
    let Statement::CreateTableAs {
        name,
        if_not_exists,
        column_names,
        with_no_data,
        ..
    } = first(
        "CREATE TABLE IF NOT EXISTS app.copy (renamed, \"Mixed\") AS \
         SELECT 1, 2, 3 WITH NO DATA",
    )
    else {
        panic!("expected CREATE TABLE AS");
    };
    assert_eq!(name, "app.copy");
    assert!(if_not_exists);
    assert_eq!(column_names, ["renamed", "Mixed"]);
    assert!(with_no_data);

    let Statement::CreateTableAs { with_no_data, .. } =
        first("CREATE TABLE populated AS SELECT 1 WITH DATA")
    else {
        panic!("expected CREATE TABLE AS");
    };
    assert!(!with_no_data);
}

#[test]
fn select_into_lowers_to_the_create_table_as_contract() {
    let Statement::CreateTableAs {
        name,
        if_not_exists,
        column_names,
        with_no_data,
        body,
        ..
    } = first("SELECT 1::smallint AS value INTO app.\"Copied\"")
    else {
        panic!("expected SELECT INTO to lower as CREATE TABLE AS");
    };
    assert_eq!(name, "app.\"Copied\"");
    assert!(!if_not_exists);
    assert!(column_names.is_empty());
    assert!(!with_no_data);
    assert_eq!(body.projections.len(), 1);
    assert_eq!(body.projections[0].alias.as_deref(), Some("value"));

    let Statement::CreateTableAs { name, body, .. } = first(
        "SELECT 1 AS value INTO union_copy \
         UNION ALL SELECT 2",
    ) else {
        panic!("expected set-operation SELECT INTO");
    };
    assert_eq!(name, "union_copy");
    assert!(body.set_op.is_some());

    let Statement::Prepare { body, .. } = first(
        "PREPARE make_copy AS \
         SELECT 7::smallint AS value INTO prepared_copy",
    ) else {
        panic!("expected PREPARE");
    };
    assert!(matches!(*body, Statement::CreateTableAs { .. }));
}

#[test]
fn create_view_preserves_positional_column_names() {
    let Statement::CreateView {
        name,
        column_names,
        or_replace,
        ..
    } = first("CREATE OR REPLACE VIEW app.labels (renamed, \"Mixed.Name\") AS SELECT 1, 2, 3")
    else {
        panic!("expected CREATE VIEW");
    };
    assert_eq!(name, "app.labels");
    assert_eq!(column_names, ["renamed", "Mixed.Name"]);
    assert!(or_replace);
}

#[test]
fn direct_unknown_literal_casts_are_validated_during_analysis() {
    let error = compile("SELECT 'bad'::integer").unwrap_err();
    assert_eq!(error.sqlstate(), Some("22P02"));

    compile("SELECT ('bad'::text)::integer").unwrap();
    compile("SELECT 999999999999::integer").unwrap();
}

#[test]
fn relation_forms_and_options_preserve_lifecycle_semantics() {
    let Statement::CreateTable(temporary) =
        first("CREATE TEMP TABLE temp_t (id INTEGER) ON COMMIT DELETE ROWS")
    else {
        panic!("expected CREATE TEMP TABLE");
    };
    assert_eq!(
        temporary.persistence,
        crate::ast::RelationPersistence::Temporary
    );
    assert_eq!(temporary.on_commit, crate::ast::OnCommitAction::DeleteRows);

    let Statement::CreateTable(unlogged) = first("CREATE UNLOGGED TABLE unlogged_t (id INTEGER)")
    else {
        panic!("expected CREATE UNLOGGED TABLE");
    };
    assert_eq!(
        unlogged.persistence,
        crate::ast::RelationPersistence::Unlogged
    );

    let Statement::CreateView {
        persistence,
        options,
        ..
    } = first(
        "CREATE TEMP VIEW temp_v WITH (security_barrier=true) AS SELECT 1 WITH LOCAL CHECK OPTION",
    )
    else {
        panic!("expected CREATE TEMP VIEW");
    };
    assert_eq!(persistence, crate::ast::RelationPersistence::Temporary);
    assert_eq!(
        options,
        [
            ("security_barrier".into(), "true".into()),
            ("check_option".into(), "local".into()),
        ]
    );

    assert!(matches!(
        first("CREATE MATERIALIZED VIEW materialized WITH (fillfactor=80) AS SELECT 1 WITH NO DATA"),
        Statement::CreateMaterializedView { with_no_data: true, options, .. }
            if options == [("fillfactor".into(), "80".into())]
    ));
    assert!(matches!(
        first("CREATE TEMP TABLE temp_as AS SELECT 1"),
        Statement::CreateTableAs {
            persistence: crate::ast::RelationPersistence::Temporary,
            ..
        }
    ));
    assert!(matches!(
        first("CREATE TEMP SEQUENCE temp_sequence"),
        Statement::CreateSequence(crate::ast::CreateSequence {
            persistence: crate::ast::RelationPersistence::Temporary,
            ..
        })
    ));
    assert!(matches!(
        first("ALTER VIEW temp_v SET (security_invoker=on)"),
        Statement::AlterViewOptions(crate::ast::AlterViewOptionsStmt {
            kind: crate::ast::AlterViewKind::View,
            action: crate::ast::AlterViewOptionsAction::Set(options),
            ..
        }) if options == [("security_invoker".into(), "on".into())]
    ));
    assert!(matches!(
        first("ALTER VIEW temp_v RESET (security_barrier)"),
        Statement::AlterViewOptions(crate::ast::AlterViewOptionsStmt {
            action: crate::ast::AlterViewOptionsAction::Reset(options),
            ..
        }) if options == ["security_barrier"]
    ));
}

#[test]
fn unsupported_create_ddl_never_loses_remaining_envelope_semantics() {
    for (sql, expected) in [
        (
            "CREATE TABLE inherited (id INTEGER) INHERITS (parent)",
            "INHERITS",
        ),
        (
            "CREATE TABLE optioned (id INTEGER) WITH (fillfactor = 70)",
            "storage options",
        ),
        (
            "CREATE TABLE spaced (id INTEGER) TABLESPACE fastspace",
            "TABLESPACE",
        ),
        (
            "CREATE TABLE accessed (id INTEGER) USING heap",
            "access methods",
        ),
        (
            "CREATE SCHEMA owned AUTHORIZATION CURRENT_USER",
            "AUTHORIZATION",
        ),
        (
            "CREATE SCHEMA bundled CREATE TABLE child (id INTEGER)",
            "schema elements",
        ),
        (
            "CREATE UNLOGGED VIEW unlogged_v AS SELECT 1",
            "cannot be unlogged",
        ),
        (
            "CREATE TEMP MATERIALIZED VIEW materialized AS SELECT 1",
            "syntax error",
        ),
    ] {
        let error = compile(sql).expect_err(sql);
        assert!(
            error.to_string().contains(expected),
            "unexpected error for {sql}: {error}"
        );
    }
}
