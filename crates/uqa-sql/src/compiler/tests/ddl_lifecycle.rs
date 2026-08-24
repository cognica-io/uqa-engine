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
        alter.actions.as_slice(),
        [AlterTableAction::AddKeyConstraint { constraint }]
            if constraint.name.as_deref() == Some("labels_tenant_slug_key")
                && constraint.kind == TableKeyConstraintKind::Unique
                && constraint.columns == ["tenant", "slug"]
    ));
}

#[test]
fn alter_table_constraint_lifecycle_preserves_every_ordered_action() {
    let Statement::AlterTable(alter) = first(
        "ALTER TABLE child \
         ADD CONSTRAINT score_ck CHECK (score > 0) NOT VALID, \
         ADD CONSTRAINT parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED NOT VALID, \
         ADD CONSTRAINT label_nn NOT NULL label NOT VALID NO INHERIT, \
         VALIDATE CONSTRAINT score_ck, \
         ALTER CONSTRAINT parent_fk NOT ENFORCED, \
         ALTER CONSTRAINT parent_fk DEFERRABLE INITIALLY DEFERRED, \
         ALTER CONSTRAINT label_nn NO INHERIT, \
         DROP CONSTRAINT score_ck CASCADE",
    ) else {
        panic!("expected ALTER TABLE");
    };
    assert_eq!(alter.actions.len(), 8);
    assert!(matches!(
        &alter.actions[0],
        AlterTableAction::AddCheckConstraint { constraint }
            if constraint.name.as_deref() == Some("score_ck")
                && constraint.enforced
                && !constraint.validated
    ));
    assert!(matches!(
        &alter.actions[1],
        AlterTableAction::AddForeignKeyConstraint { constraint }
            if constraint.name.as_deref() == Some("parent_fk")
                && constraint.enforced
                && !constraint.validated
                && constraint.deferrable
                && constraint.initially_deferred
    ));
    assert!(matches!(
        &alter.actions[2],
        AlterTableAction::AddNotNullConstraint { name, column, validated, no_inherit }
            if name.as_deref() == Some("label_nn")
                && column == "label"
                && !validated
                && *no_inherit
    ));
    assert!(matches!(
        &alter.actions[3],
        AlterTableAction::ValidateConstraint { name } if name == "score_ck"
    ));
    assert!(matches!(
        &alter.actions[4],
        AlterTableAction::AlterConstraint { name, enforceability: Some(false), .. }
            if name == "parent_fk"
    ));
    assert!(matches!(
        &alter.actions[5],
        AlterTableAction::AlterConstraint {
            name,
            deferrability: Some((true, true)),
            ..
        } if name == "parent_fk"
    ));
    assert!(matches!(
        &alter.actions[6],
        AlterTableAction::AlterConstraint { name, no_inherit: Some(true), .. }
            if name == "label_nn"
    ));
    assert!(matches!(
        &alter.actions[7],
        AlterTableAction::DropConstraint { name, cascade: true, .. } if name == "score_ck"
    ));
}

#[test]
fn alter_constraint_not_valid_reports_postgresql_feature_state() {
    let error = compile("ALTER TABLE child ALTER CONSTRAINT parent_fk NOT VALID").unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert!(error
        .to_string()
        .contains("constraints cannot be altered to be NOT VALID"));
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
fn unsupported_create_ddl_never_loses_lifecycle_semantics() {
    for (sql, expected) in [
        ("CREATE TEMP TABLE temp_t (id INTEGER)", "TEMPORARY"),
        ("CREATE UNLOGGED TABLE unlogged_t (id INTEGER)", "UNLOGGED"),
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
        ("CREATE TEMP VIEW temp_v AS SELECT 1", "TEMPORARY"),
        (
            "CREATE VIEW checked AS SELECT 1 WITH LOCAL CHECK OPTION",
            "CHECK OPTION",
        ),
        (
            "CREATE VIEW optioned_v WITH (security_barrier = true) AS SELECT 1",
            "options",
        ),
        (
            "CREATE MATERIALIZED VIEW materialized AS SELECT 1",
            "MATERIALIZED VIEW",
        ),
        ("CREATE TEMP TABLE temp_as AS SELECT 1", "TEMPORARY"),
        ("CREATE TEMP SEQUENCE temp_sequence", "TEMPORARY"),
    ] {
        let error = compile(sql).expect_err(sql);
        assert!(
            matches!(&error, SQLError::Unsupported(message) if message.contains(expected)),
            "unexpected error for {sql}: {error}"
        );
    }
}
