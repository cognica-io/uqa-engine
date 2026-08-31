//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn scalar_subquery_view_engine() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE scalar_view_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO scalar_view_base VALUES (1, 10), (2, 20), (3, 30);
         CREATE TABLE scalar_view_config (cutoff INTEGER);
         INSERT INTO scalar_view_config VALUES (15);
         CREATE TABLE scalar_view_detail (id INTEGER, score INTEGER);
         INSERT INTO scalar_view_detail VALUES (1, 3), (1, 4), (2, 8);
         CREATE TABLE scalar_image_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO scalar_image_base VALUES (1, 4);
         CREATE TABLE scalar_image_detail (score INTEGER);
         INSERT INTO scalar_image_detail VALUES (3), (5), (8);
         CREATE TABLE scalar_snapshot_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO scalar_snapshot_base VALUES (1, 10), (2, 20);
         CREATE VIEW scalar_projection_view AS
           SELECT base.id, base.value,
             (SELECT max(cutoff) FROM scalar_view_config) AS ceiling
           FROM scalar_view_base AS base;
         CREATE VIEW scalar_correlated_view AS
           SELECT base.id, base.value,
             (SELECT max(detail.score) FROM scalar_view_detail AS detail
              WHERE detail.id = base.id) AS best
           FROM scalar_view_base AS base;
         CREATE VIEW scalar_predicate_view AS
           SELECT base.id, base.value FROM scalar_view_base AS base
           WHERE base.value > (SELECT max(cutoff) FROM scalar_view_config);
         CREATE VIEW scalar_nested_view AS
           SELECT id, value, best FROM scalar_correlated_view WHERE best IS NOT NULL;
         CREATE VIEW scalar_collision_view AS
           SELECT base.id, base.value,
             (SELECT max(target.score) FROM scalar_view_detail AS target
              WHERE target.id = base.id) AS best
           FROM scalar_view_base AS base;
         CREATE VIEW scalar_exists_view AS
           SELECT base.id, base.value FROM scalar_view_base AS base
           WHERE EXISTS (
             SELECT 1 FROM scalar_view_detail AS detail WHERE detail.id = base.id
           );
         CREATE VIEW scalar_in_view AS
           SELECT base.id, base.value FROM scalar_view_base AS base
           WHERE base.id IN (SELECT detail.id FROM scalar_view_detail AS detail);
         CREATE VIEW scalar_unqualified_view AS
           SELECT base.id, base.value, (SELECT value + 1) AS next_value
           FROM scalar_view_base AS base;
         CREATE VIEW scalar_cte_expression_view AS
           SELECT base.id, base.value,
             (WITH cutoff_row AS (
                SELECT max(cutoff) AS cutoff FROM scalar_view_config
              ) SELECT cutoff FROM cutoff_row) AS ceiling
           FROM scalar_view_base AS base;
         CREATE VIEW scalar_snapshot_view AS
           SELECT base.id, base.value,
             (SELECT max(value) FROM scalar_snapshot_base) AS peak
           FROM scalar_snapshot_base AS base;
         CREATE VIEW scalar_image_view AS
           SELECT base.id, base.value,
             (SELECT count(*) FROM scalar_image_detail AS detail
              WHERE detail.score <= base.value) AS eligible
           FROM scalar_image_base AS base;
         CREATE VIEW scalar_checked_view AS
           SELECT base.id, base.value FROM scalar_view_base AS base
           WHERE base.value > (SELECT max(cutoff) FROM scalar_view_config)
           WITH LOCAL CHECK OPTION;
         CREATE VIEW scalar_with_view AS
           WITH marker AS (SELECT 1 AS value)
           SELECT base.id, base.value FROM scalar_view_base AS base",
    );
    engine
}

#[test]
fn assert_scalar_subquery_views_remain_automatically_updatable() {
    let engine = scalar_subquery_view_engine();
    assert_scalar_subquery_view_catalog_and_projection(&engine);
    assert_scalar_subquery_view_correlations_and_snapshots(&engine);
    assert_scalar_subquery_view_images_predicates_and_checks(&engine);
    assert_scalar_subquery_view_merge_rules_and_boundaries(&engine);
}

fn assert_scalar_subquery_view_catalog_and_projection(engine: &Engine) {
    let views = exec(
        engine,
        "SELECT table_name, is_updatable, is_insertable_into
         FROM information_schema.views
         WHERE table_name IN (
           'scalar_projection_view', 'scalar_correlated_view',
           'scalar_predicate_view', 'scalar_nested_view', 'scalar_with_view'
         ) ORDER BY table_name",
    );
    assert_eq!(views.rows.len(), 5);
    for row in &views.rows[..4] {
        assert_eq!(row["is_updatable"], Value::Str("YES".into()));
        assert_eq!(row["is_insertable_into"], Value::Str("YES".into()));
    }
    assert_eq!(views.rows[4]["is_updatable"], Value::Str("NO".into()));
    assert_eq!(views.rows[4]["is_insertable_into"], Value::Str("NO".into()));
    let columns = exec(
        engine,
        "SELECT column_name, is_updatable FROM information_schema.columns
         WHERE table_name = 'scalar_projection_view' ORDER BY ordinal_position",
    );
    assert_eq!(columns.rows[0]["is_updatable"], Value::Str("YES".into()));
    assert_eq!(columns.rows[1]["is_updatable"], Value::Str("YES".into()));
    assert_eq!(columns.rows[2]["is_updatable"], Value::Str("NO".into()));

    let inserted = exec(
        engine,
        "INSERT INTO scalar_projection_view (id, value) VALUES (4, 40)
         RETURNING id, value, ceiling",
    );
    assert_eq!(inserted.rows[0]["id"], Value::Int(4));
    assert_eq!(inserted.rows[0]["value"], Value::Int(40));
    assert_eq!(inserted.rows[0]["ceiling"], Value::Int(15));
    let updated = exec(
        engine,
        "UPDATE scalar_projection_view SET value = 41 WHERE id = 4
         RETURNING id, value, ceiling",
    );
    assert_eq!(updated.rows[0]["value"], Value::Int(41));
    assert_eq!(updated.rows[0]["ceiling"], Value::Int(15));
    let deleted = exec(
        engine,
        "DELETE FROM scalar_projection_view WHERE id = 4
         RETURNING id, value, ceiling",
    );
    assert_eq!(deleted.rows[0]["value"], Value::Int(41));
    assert_eq!(deleted.rows[0]["ceiling"], Value::Int(15));
}

fn assert_scalar_subquery_view_correlations_and_snapshots(engine: &Engine) {
    let correlated = exec(
        engine,
        "UPDATE scalar_correlated_view SET value = 11 WHERE id = 1
         RETURNING id, value, best",
    );
    assert_eq!(correlated.rows[0]["value"], Value::Int(11));
    assert_eq!(correlated.rows[0]["best"], Value::Int(4));
    let nested = exec(
        engine,
        "UPDATE scalar_nested_view SET value = 12 WHERE id = 1
         RETURNING id, value, best",
    );
    assert_eq!(nested.rows[0]["value"], Value::Int(12));
    assert_eq!(nested.rows[0]["best"], Value::Int(4));
    let nested_reference = exec(
        engine,
        "UPDATE scalar_correlated_view AS target SET value = 13
         WHERE (SELECT target.best) = 4
         RETURNING id, value, best",
    );
    assert_eq!(nested_reference.rows.len(), 1);
    assert_eq!(nested_reference.rows[0]["value"], Value::Int(13));
    let collision = exec(
        engine,
        "UPDATE scalar_collision_view AS target SET value = 14 WHERE id = 1
         RETURNING id, value, best",
    );
    assert_eq!(collision.rows[0]["best"], Value::Int(4));
    let exists = exec(
        engine,
        "UPDATE scalar_exists_view SET value = value + 1 WHERE id = 2
         RETURNING id, value",
    );
    assert_eq!(exists.rows[0]["value"], Value::Int(21));
    let in_subquery = exec(
        engine,
        "UPDATE scalar_in_view SET value = value WHERE id = 2 RETURNING id, value",
    );
    assert_eq!(in_subquery.rows[0]["value"], Value::Int(21));
    let unqualified = exec(
        engine,
        "UPDATE scalar_unqualified_view SET value = value WHERE id = 1
         RETURNING next_value",
    );
    assert_eq!(unqualified.rows[0]["next_value"], Value::Int(15));
    let cte_expression = exec(
        engine,
        "UPDATE scalar_cte_expression_view SET value = value WHERE id = 1
         RETURNING ceiling",
    );
    assert_eq!(cte_expression.rows[0]["ceiling"], Value::Int(15));
    let snapshot = exec(
        engine,
        "UPDATE scalar_snapshot_view SET value = value + 100
         RETURNING id, value, peak",
    );
    assert_eq!(snapshot.rows.len(), 2);
    assert!(snapshot
        .rows
        .iter()
        .all(|row| row["peak"] == Value::Int(20)));
}

fn assert_scalar_subquery_view_images_predicates_and_checks(engine: &Engine) {
    let row_images = exec(
        engine,
        "UPDATE scalar_image_view SET value = 8 WHERE id = 1
         RETURNING WITH (OLD AS before, NEW AS after)
           before.value AS old_value, after.value AS new_value,
           before.eligible AS old_eligible, after.eligible AS new_eligible,
           eligible AS current_eligible",
    );
    assert_eq!(row_images.rows[0]["old_value"], Value::Int(4));
    assert_eq!(row_images.rows[0]["new_value"], Value::Int(8));
    assert_eq!(row_images.rows[0]["old_eligible"], Value::Int(1));
    assert_eq!(row_images.rows[0]["new_eligible"], Value::Int(3));
    assert_eq!(row_images.rows[0]["current_eligible"], Value::Int(3));

    let inserted_outside = exec(
        engine,
        "INSERT INTO scalar_predicate_view VALUES (4, 12) RETURNING id, value",
    );
    assert_eq!(inserted_outside.rows[0]["value"], Value::Int(12));
    let moved_outside = exec(
        engine,
        "UPDATE scalar_predicate_view SET value = 12 WHERE id = 2 RETURNING id, value",
    );
    assert_eq!(moved_outside.rows[0]["value"], Value::Int(12));
    let deleted_visible = exec(
        engine,
        "DELETE FROM scalar_predicate_view WHERE id = 3 RETURNING id, value",
    );
    assert_eq!(deleted_visible.rows[0]["id"], Value::Int(3));

    exec(engine, "INSERT INTO scalar_checked_view VALUES (6, 20)");
    let check_error = engine
        .sql(
            "UPDATE scalar_checked_view SET value = 14 WHERE id = 6",
            &[],
        )
        .unwrap_err();
    assert_eq!(check_error.sqlstate(), Some("44000"));
    assert_eq!(
        exec(engine, "SELECT value FROM scalar_view_base WHERE id = 6").rows[0]["value"],
        Value::Int(20)
    );
}

fn assert_scalar_subquery_view_merge_rules_and_boundaries(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE scalar_merge_source (id INTEGER, value INTEGER);
         INSERT INTO scalar_merge_source VALUES (1, 7)",
    );
    let merged = exec(
        engine,
        "MERGE INTO scalar_image_view AS target
         USING scalar_merge_source AS source ON target.id = source.id
         WHEN MATCHED THEN UPDATE SET value = source.value
         RETURNING WITH (OLD AS before, NEW AS after)
           merge_action() AS action, target.id,
           before.eligible AS old_eligible, after.eligible AS new_eligible,
           target.eligible AS current_eligible",
    );
    assert_eq!(merged.rows[0]["action"], Value::Str("UPDATE".into()));
    assert_eq!(merged.rows[0]["old_eligible"], Value::Int(3));
    assert_eq!(merged.rows[0]["new_eligible"], Value::Int(2));
    assert_eq!(merged.rows[0]["current_eligible"], Value::Int(2));

    exec(
        engine,
        "CREATE TABLE scalar_rule_log (
           old_eligible INTEGER, new_eligible INTEGER
         );
         CREATE RULE scalar_image_update_rule AS ON UPDATE TO scalar_image_view
           DO ALSO INSERT INTO scalar_rule_log
             VALUES (OLD.eligible, NEW.eligible)",
    );
    exec(
        engine,
        "UPDATE scalar_image_view SET value = 4 WHERE id = 1",
    );
    let rule_row = exec(
        engine,
        "SELECT old_eligible, new_eligible FROM scalar_rule_log",
    );
    assert_eq!(rule_row.rows[0]["old_eligible"], Value::Int(2));
    assert_eq!(rule_row.rows[0]["new_eligible"], Value::Int(2));

    let error = engine
        .sql("UPDATE scalar_with_view SET value = 99 WHERE id = 1", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("55000"));
}

pub(super) fn assert_unqualified_system_columns_use_target_qualification_cardinality() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE system_qualification_base (id INTEGER PRIMARY KEY);
         INSERT INTO system_qualification_base VALUES (1), (2);
         CREATE TABLE system_qualification_log (event TEXT);
         CREATE RULE system_qualification_update AS ON UPDATE TO system_qualification_base
           DO ALSO INSERT INTO system_qualification_log VALUES ('update')",
    );
    exec(
        &engine,
        "UPDATE system_qualification_base AS target SET id = target.id
         FROM (VALUES (1), (2), (3)) AS source(value)
         WHERE tableoid = 'system_qualification_base'::regclass",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS total FROM system_qualification_log"
        )
        .rows[0]["total"],
        Value::Int(6)
    );
    exec(
        &engine,
        "TRUNCATE system_qualification_log;
         CREATE RULE system_qualification_delete AS ON DELETE TO system_qualification_base
           DO ALSO INSERT INTO system_qualification_log VALUES ('delete');
         DELETE FROM system_qualification_base
         USING (VALUES (1), (2), (3)) AS source(value)
         WHERE tableoid = 'system_qualification_base'::regclass",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS total FROM system_qualification_log"
        )
        .rows[0]["total"],
        Value::Int(6)
    );
}

pub(super) fn assert_correlated_source_only_names_remain_source_bound() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE correlated_source_base (
           id INTEGER PRIMARY KEY,
           shown TEXT NOT NULL,
           secret TEXT NOT NULL
         );
         INSERT INTO correlated_source_base VALUES (1, 'before', 'base');
         CREATE VIEW correlated_source_view AS SELECT id, shown FROM correlated_source_base",
    );
    let updated = exec(
        &engine,
        "UPDATE correlated_source_view SET shown = (SELECT secret)
         FROM (SELECT 'source' AS secret) AS source
         WHERE id = 1 RETURNING shown",
    );
    assert_eq!(updated.rows[0]["shown"], Value::Str("source".into()));
}

pub(super) fn assert_automatic_view_subqueries_keep_the_complete_dml_scope() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE scoped_view_base (id INTEGER PRIMARY KEY, label TEXT NOT NULL);
         CREATE TABLE scoped_view_source (id INTEGER PRIMARY KEY, label TEXT NOT NULL);
         INSERT INTO scoped_view_source VALUES (1, 'from-source'), (2, 'delete-source');
         CREATE VIEW scoped_view (item_id, item_label) AS
           SELECT id, label FROM scoped_view_base",
    );
    exec(&engine, "INSERT INTO scoped_view VALUES (1, 'before')");
    let conflicted = exec(
        &engine,
        "INSERT INTO scoped_view VALUES (1, 'from-excluded')
         ON CONFLICT (item_id) DO UPDATE
           SET item_label = (SELECT excluded.item_label)
         RETURNING item_id, item_label",
    );
    assert_eq!(
        conflicted.rows[0]["item_label"],
        Value::Str("from-excluded".into())
    );

    let updated = exec(
        &engine,
        "UPDATE scoped_view AS target
         SET item_label = source.label
         FROM scoped_view_source AS source
         WHERE target.item_id = source.id AND source.id = 1
         RETURNING WITH (OLD AS before, NEW AS after)
           (SELECT source.label) AS source_label,
           (SELECT before.item_label) AS old_label,
           (SELECT after.item_label) AS new_label",
    );
    assert_eq!(
        updated.rows[0]["source_label"],
        Value::Str("from-source".into())
    );
    assert_eq!(
        updated.rows[0]["old_label"],
        Value::Str("from-excluded".into())
    );
    assert_eq!(
        updated.rows[0]["new_label"],
        Value::Str("from-source".into())
    );

    exec(
        &engine,
        "INSERT INTO scoped_view VALUES (2, 'before-delete')",
    );
    let deleted = exec(
        &engine,
        "DELETE FROM scoped_view AS target
         USING scoped_view_source AS source
         WHERE target.item_id = source.id AND source.id = 2
         RETURNING (SELECT source.label) AS source_label,
                   (SELECT target.item_label) AS deleted_label",
    );
    assert_eq!(
        deleted.rows[0]["source_label"],
        Value::Str("delete-source".into())
    );
    assert_eq!(
        deleted.rows[0]["deleted_label"],
        Value::Str("before-delete".into())
    );
}

pub(super) fn assert_correlated_view_references_use_the_public_row_type() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE correlated_view_base (
           id INTEGER PRIMARY KEY,
           visible TEXT NOT NULL,
           secret TEXT NOT NULL
         );
         INSERT INTO correlated_view_base VALUES (1, 'before', 'hidden');
         CREATE VIEW correlated_view (item_id, visible) AS
           SELECT id, visible FROM correlated_view_base",
    );
    let hidden = engine
        .sql(
            "UPDATE correlated_view AS target SET visible = 'leaked'
             WHERE EXISTS (SELECT 1 WHERE target.secret = 'hidden')",
            &[],
        )
        .unwrap_err();
    assert_eq!(hidden.sqlstate(), Some("42703"));
    let updated = exec(
        &engine,
        "UPDATE correlated_view AS target SET visible = 'after'
         WHERE EXISTS (SELECT 1 WHERE target.item_id = 1)
         RETURNING item_id, visible",
    );
    assert_eq!(updated.rows.len(), 1);
    assert_eq!(updated.rows[0]["item_id"], Value::Int(1));
    assert_eq!(updated.rows[0]["visible"], Value::Str("after".into()));
}

pub(super) fn assert_unaliased_derived_source_keeps_source_only_names() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE unaliased_source_base (
           id INTEGER PRIMARY KEY,
           shown TEXT NOT NULL,
           secret TEXT NOT NULL
         );
         INSERT INTO unaliased_source_base VALUES (1, 'before', 'hidden');
         CREATE VIEW unaliased_source_view AS SELECT id, shown FROM unaliased_source_base",
    );
    let updated = exec(
        &engine,
        "UPDATE unaliased_source_view SET shown = secret
         FROM (SELECT 'source' AS secret)
         WHERE id = 1 RETURNING shown",
    );
    assert_eq!(updated.rows[0]["shown"], Value::Str("source".into()));
}
