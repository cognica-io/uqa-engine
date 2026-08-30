//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 automatically updatable view coverage.

use super::*;

fn automatic_view_engine() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE automatic_base (
            id INTEGER PRIMARY KEY,
            value TEXT NOT NULL,
            visible BOOLEAN NOT NULL DEFAULT true,
            quantity INTEGER NOT NULL DEFAULT 7
        )",
    );
    exec(
        &engine,
        "CREATE VIEW automatic_items (item_id, label, visible, doubled) AS
         SELECT id, value, visible, quantity * 2 FROM automatic_base WHERE visible",
    );
    engine
}

#[test]
fn automatically_updatable_views_match_postgresql_18() {
    assert_simple_view_insert_defaults_upsert_and_computed_columns();
    assert_update_from_delete_using_returning_and_visibility();
    assert_view_row_type_is_the_dml_name_boundary();
    assert_source_old_and_new_aliases_remain_source_relations();
    assert_view_rules_precede_automatic_rewrite();
    assert_nested_view_rules_run_at_each_rewrite_layer();
    assert_nested_instead_rules_stop_lower_rewrite_layers();
    assert_rule_and_trigger_rewrite_order();
    assert_update_delete_rule_and_trigger_rewrite_order();
    assert_nested_insert_rule_suppression_and_order();
    assert_suppressed_rule_insert_does_not_prepare_base_row();
    assert_nested_rule_returning_provider_and_lazy_projection();
    assert_rule_projection_defaults_returning_and_command_tags();
    assert_rule_suppression_defers_expressions_and_statement_triggers();
    assert_rule_insert_images_conflicts_and_lazy_sources();
    assert_rule_action_cardinality_matches_postgresql_18();
    assert_suppressed_nested_and_direct_view_dml_is_lazy();
    assert_rule_condition_case_projection_is_lazy();
    assert_automatic_view_subqueries_keep_the_complete_dml_scope();
    assert_conditional_instead_rules_do_not_make_views_updatable();
    assert_nested_conditional_instead_rule_rejects_statement();
    assert_view_rule_actions_with_duplicate_user_ids_survive_reopen();
    assert_correlated_view_references_use_the_public_row_type();
    assert_unaliased_derived_source_keeps_source_only_names();
    assert_correlated_source_only_names_remain_source_bound();
    assert_base_triggers_replace_view_statement_triggers();
    assert_local_and_cascaded_check_options();
    assert_nested_views_preserve_aliases_and_defaults();
    assert_view_star_row_type_is_fixed_at_creation();
    assert_partition_tableoid_uses_the_physical_relation();
    assert_non_updatable_views_and_catalog_flags();
    assert_rule_updatability_catalog_flags();
    assert_check_option_error_order_and_mapped_duplicates();
    assert_check_option_definition_over_non_updatable_source();
    assert_only_partition_view_insert_routes_to_a_partition();
    assert_nested_rule_backed_computed_columns();
    assert_nested_nonautomatic_rule_backed_view_executes();
    assert_nonautomatic_rule_boundary_preserves_outer_layers();
    assert_rule_backed_view_inputs_are_evaluated_lazily();
    assert_rule_catalog_flags_ignore_replication_mode();
    assert_select_star_without_a_relation_is_rejected();
    assert_unqualified_system_columns_use_target_qualification_cardinality();
}

fn assert_check_option_definition_over_non_updatable_source() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE check_source_base (id INTEGER PRIMARY KEY);
         CREATE VIEW check_source_inner AS SELECT DISTINCT id FROM check_source_base;
         CREATE VIEW check_source_outer AS
           SELECT id FROM check_source_inner WHERE id > 0 WITH CHECK OPTION",
    );
    let flags = exec(
        &engine,
        "SELECT is_updatable, is_insertable_into
         FROM information_schema.views
         WHERE table_name = 'check_source_outer'",
    );
    assert_eq!(flags.rows[0]["is_updatable"], Value::Str("NO".into()));
    assert_eq!(flags.rows[0]["is_insertable_into"], Value::Str("NO".into()));
}

fn assert_only_partition_view_insert_routes_to_a_partition() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE only_partition_base (id INTEGER, value INTEGER) PARTITION BY RANGE (id);
         CREATE TABLE only_partition_child PARTITION OF only_partition_base FOR VALUES FROM (0) TO (10);
         CREATE VIEW only_partition_view AS SELECT id, value FROM ONLY only_partition_base;
         INSERT INTO only_partition_view VALUES (1, 10)",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS total FROM only_partition_child"
        )
        .rows[0]["total"],
        Value::Int(1)
    );
    assert_eq!(
        exec(&engine, "SELECT count(*) AS total FROM only_partition_view").rows[0]["total"],
        Value::Int(0)
    );
}

fn assert_nested_rule_backed_computed_columns() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_computed_base (id INTEGER PRIMARY KEY, value INTEGER);
         CREATE TABLE nested_computed_log (value INTEGER);
         CREATE VIEW nested_computed_inner AS
           SELECT id, value * 2 AS doubled FROM nested_computed_base;
         CREATE RULE nested_computed_insert AS ON INSERT TO nested_computed_inner
           DO INSTEAD INSERT INTO nested_computed_log VALUES (NEW.doubled);
         CREATE RULE nested_computed_update AS ON UPDATE TO nested_computed_inner
           DO INSTEAD INSERT INTO nested_computed_log VALUES (NEW.doubled);
         CREATE VIEW nested_computed_outer AS SELECT id, doubled FROM nested_computed_inner",
    );
    assert_eq!(
        exec(&engine, "INSERT INTO nested_computed_outer VALUES (1, 8)").affected_rows,
        1
    );
    exec(&engine, "INSERT INTO nested_computed_base VALUES (2, 2)");
    assert_eq!(
        exec(
            &engine,
            "UPDATE nested_computed_outer SET doubled = 12 WHERE id = 2"
        )
        .affected_rows,
        0
    );
    let logged = exec(
        &engine,
        "SELECT value FROM nested_computed_log ORDER BY value",
    );
    assert_eq!(logged.rows[0]["value"], Value::Int(8));
    assert_eq!(logged.rows[1]["value"], Value::Int(12));
    let flags = exec(
        &engine,
        "SELECT table_name, is_updatable, is_insertable_into
         FROM information_schema.views
         WHERE table_name IN ('nested_computed_inner', 'nested_computed_outer')
         ORDER BY table_name",
    );
    for row in flags.rows {
        assert_eq!(row["is_updatable"], Value::Str("YES".into()));
        assert_eq!(row["is_insertable_into"], Value::Str("YES".into()));
    }
}

fn assert_nested_nonautomatic_rule_backed_view_executes() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_aggregate_base (value INTEGER);
         INSERT INTO nested_aggregate_base VALUES (1), (2);
         CREATE TABLE nested_aggregate_log (value INTEGER);
         CREATE VIEW nested_aggregate_inner AS
           SELECT sum(value)::INTEGER AS total FROM nested_aggregate_base;
         CREATE RULE nested_aggregate_update AS ON UPDATE TO nested_aggregate_inner
           DO INSTEAD INSERT INTO nested_aggregate_log VALUES (NEW.total);
         CREATE VIEW nested_aggregate_outer AS SELECT total FROM nested_aggregate_inner",
    );
    assert_eq!(
        exec(
            &engine,
            "UPDATE nested_aggregate_outer SET total = 9 WHERE total = 3"
        )
        .affected_rows,
        0
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM nested_aggregate_log").rows[0]["value"],
        Value::Int(9)
    );
    let flags = exec(
        &engine,
        "SELECT table_name, is_updatable, is_insertable_into
         FROM information_schema.views
         WHERE table_name IN ('nested_aggregate_inner', 'nested_aggregate_outer')
         ORDER BY table_name",
    );
    for row in flags.rows {
        assert_eq!(row["is_updatable"], Value::Str("NO".into()));
        assert_eq!(row["is_insertable_into"], Value::Str("NO".into()));
    }
}

fn assert_nonautomatic_rule_boundary_preserves_outer_layers() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE rule_boundary_base (id INTEGER);
         INSERT INTO rule_boundary_base VALUES (1), (2);
         CREATE TABLE rule_boundary_log (
           seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
           event TEXT,
           value INTEGER
         );
         CREATE VIEW rule_boundary_insert_inner AS
           SELECT DISTINCT id FROM rule_boundary_base;
         CREATE RULE rule_boundary_insert_inner_rule AS ON INSERT TO rule_boundary_insert_inner
           DO INSTEAD INSERT INTO rule_boundary_log(event, value) VALUES ('inner-insert', NEW.id);
         CREATE VIEW rule_boundary_insert_outer AS SELECT id FROM rule_boundary_insert_inner;
         CREATE RULE rule_boundary_insert_outer_rule AS ON INSERT TO rule_boundary_insert_outer
           DO ALSO INSERT INTO rule_boundary_log(event, value) VALUES ('outer-insert', NEW.id);
         CREATE VIEW rule_boundary_update_inner AS
           SELECT sum(id)::INTEGER AS total FROM rule_boundary_base;
         CREATE RULE rule_boundary_update_inner_rule AS ON UPDATE TO rule_boundary_update_inner
           DO INSTEAD INSERT INTO rule_boundary_log(event, value) VALUES ('inner-update', NEW.total);
         CREATE VIEW rule_boundary_update_outer AS SELECT total FROM rule_boundary_update_inner;
         CREATE RULE rule_boundary_update_outer_rule AS ON UPDATE TO rule_boundary_update_outer
           DO ALSO INSERT INTO rule_boundary_log(event, value) VALUES ('outer-update', NEW.total);
         CREATE VIEW rule_boundary_delete_inner AS
           SELECT DISTINCT id FROM rule_boundary_base;
         CREATE RULE rule_boundary_delete_inner_rule AS ON DELETE TO rule_boundary_delete_inner
           DO INSTEAD INSERT INTO rule_boundary_log(event, value) VALUES ('inner-delete', OLD.id);
         CREATE VIEW rule_boundary_delete_outer AS SELECT id FROM rule_boundary_delete_inner;
         CREATE RULE rule_boundary_delete_outer_rule AS ON DELETE TO rule_boundary_delete_outer
           DO ALSO INSERT INTO rule_boundary_log(event, value) VALUES ('outer-delete', OLD.id)",
    );
    exec(
        &engine,
        "INSERT INTO rule_boundary_insert_outer VALUES (3);
         UPDATE rule_boundary_update_outer SET total = 9;
         DELETE FROM rule_boundary_delete_outer WHERE id = 1",
    );
    let log = exec(
        &engine,
        "SELECT event, value FROM rule_boundary_log ORDER BY seq",
    );
    assert_eq!(
        log.rows
            .iter()
            .map(|row| (row["event"].clone(), row["value"].clone()))
            .collect::<Vec<_>>(),
        vec![
            (Value::Str("inner-insert".into()), Value::Int(3)),
            (Value::Str("outer-insert".into()), Value::Int(3)),
            (Value::Str("outer-update".into()), Value::Int(9)),
            (Value::Str("inner-update".into()), Value::Int(9)),
            (Value::Str("outer-delete".into()), Value::Int(1)),
            (Value::Str("inner-delete".into()), Value::Int(1)),
        ]
    );
}

fn assert_unqualified_system_columns_use_target_qualification_cardinality() {
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

fn assert_rule_backed_view_inputs_are_evaluated_lazily() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE consumed_lazy_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO consumed_lazy_base VALUES (1, 10);
         CREATE TABLE consumed_lazy_log (event TEXT, id INTEGER);
         CREATE VIEW consumed_lazy_update AS
           SELECT id, 1 / (id - id) AS boom FROM consumed_lazy_base;
         CREATE RULE consumed_lazy_update_rule AS ON UPDATE TO consumed_lazy_update
           DO INSTEAD INSERT INTO consumed_lazy_log VALUES ('update', NEW.id);
         CREATE VIEW consumed_lazy_insert AS SELECT id, value FROM consumed_lazy_base;
         CREATE RULE consumed_lazy_insert_rule AS ON INSERT TO consumed_lazy_insert
           DO INSTEAD INSERT INTO consumed_lazy_log VALUES ('insert-select', NEW.id);
         CREATE VIEW consumed_lazy_inner AS SELECT id, value FROM consumed_lazy_base;
         CREATE RULE consumed_lazy_inner_rule AS ON INSERT TO consumed_lazy_inner
           DO INSTEAD INSERT INTO consumed_lazy_log VALUES ('nested-values', NEW.id);
         CREATE VIEW consumed_lazy_outer AS SELECT id, value FROM consumed_lazy_inner",
    );
    exec(
        &engine,
        "UPDATE consumed_lazy_update SET id = id WHERE id = 1;
         INSERT INTO consumed_lazy_insert SELECT 2, 1 / 0;
         INSERT INTO consumed_lazy_outer VALUES (3, 1 / 0)",
    );
    let logged = exec(
        &engine,
        "SELECT event, id FROM consumed_lazy_log ORDER BY id",
    );
    assert_eq!(logged.rows.len(), 3);
    assert_eq!(logged.rows[0]["id"], Value::Int(1));
    assert_eq!(logged.rows[1]["id"], Value::Int(2));
    assert_eq!(logged.rows[2]["id"], Value::Int(3));
}

fn assert_rule_catalog_flags_ignore_replication_mode() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE mode_catalog_base (id INTEGER);
         CREATE VIEW mode_catalog_view AS SELECT DISTINCT id FROM mode_catalog_base;
         CREATE RULE mode_catalog_insert AS ON INSERT TO mode_catalog_view DO INSTEAD NOTHING;
         SET session_replication_role = replica",
    );
    let flags = exec(
        &engine,
        "SELECT is_insertable_into FROM information_schema.views
         WHERE table_name = 'mode_catalog_view'",
    );
    assert_eq!(
        flags.rows[0]["is_insertable_into"],
        Value::Str("YES".into())
    );
    exec(&engine, "RESET session_replication_role");
}

fn assert_select_star_without_a_relation_is_rejected() {
    let engine = Engine::new();
    let select = engine.sql("SELECT *", &[]).unwrap_err();
    assert_eq!(select.sqlstate(), Some("42601"));
    let view = engine
        .sql("CREATE VIEW invalid_star_view AS SELECT *", &[])
        .unwrap_err();
    assert_eq!(view.sqlstate(), Some("42601"));
}

fn assert_nested_instead_rules_stop_lower_rewrite_layers() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_stop_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO nested_stop_base VALUES (1, 10), (2, 20);
         CREATE TABLE nested_stop_log (event TEXT);
         CREATE RULE nested_stop_base_update AS ON UPDATE TO nested_stop_base
           DO ALSO INSERT INTO nested_stop_log VALUES ('base-update');
         CREATE RULE nested_stop_base_delete AS ON DELETE TO nested_stop_base
           DO ALSO INSERT INTO nested_stop_log VALUES ('base-delete');
         CREATE VIEW nested_stop_low AS SELECT id, value FROM nested_stop_base;
         CREATE RULE nested_stop_low_update AS ON UPDATE TO nested_stop_low
           DO ALSO INSERT INTO nested_stop_log VALUES ('low-update');
         CREATE RULE nested_stop_low_delete AS ON DELETE TO nested_stop_low
           DO ALSO INSERT INTO nested_stop_log VALUES ('low-delete');
         CREATE VIEW nested_stop_mid AS SELECT id, value FROM nested_stop_low;
         CREATE RULE nested_stop_mid_update AS ON UPDATE TO nested_stop_mid DO INSTEAD NOTHING;
         CREATE RULE nested_stop_mid_delete AS ON DELETE TO nested_stop_mid DO INSTEAD NOTHING;
         CREATE VIEW nested_stop_top AS SELECT id, value FROM nested_stop_mid",
    );
    let updated = exec(
        &engine,
        "UPDATE nested_stop_top SET value = 11 WHERE id = 1",
    );
    let deleted = exec(&engine, "DELETE FROM nested_stop_top WHERE id = 2");
    assert_eq!(updated.affected_rows, 0);
    assert_eq!(deleted.affected_rows, 0);
    assert!(exec(&engine, "SELECT * FROM nested_stop_log")
        .rows
        .is_empty());
    assert_eq!(
        exec(&engine, "SELECT count(*) AS total FROM nested_stop_base").rows[0]["total"],
        Value::Int(2)
    );
}

fn assert_rule_insert_images_conflicts_and_lazy_sources() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE rule_image_base (id INTEGER PRIMARY KEY);
         CREATE TABLE rule_image_log (label TEXT, value INTEGER);
         CREATE VIEW rule_image_view AS SELECT id, id * 2 AS doubled FROM rule_image_base;
         CREATE RULE rule_image_null AS ON INSERT TO rule_image_view
           WHERE NEW.doubled IS NULL
           DO ALSO INSERT INTO rule_image_log VALUES ('null', NEW.doubled);
         CREATE RULE rule_image_value AS ON INSERT TO rule_image_view
           WHERE NEW.doubled IS NOT NULL
           DO ALSO INSERT INTO rule_image_log VALUES ('value', NEW.doubled)",
    );
    exec(&engine, "INSERT INTO rule_image_view(id) VALUES (3)");
    let image = exec(&engine, "SELECT label, value FROM rule_image_log");
    assert_eq!(image.rows.len(), 1);
    assert_eq!(image.rows[0]["label"], Value::Str("null".into()));
    assert_eq!(image.rows[0]["value"], Value::Null);

    exec(
        &engine,
        "CREATE TABLE conflict_rule_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO conflict_rule_base VALUES (1, 10);
         CREATE TABLE conflict_rule_log (id INTEGER);
         CREATE VIEW conflict_rule_view AS SELECT id, value FROM conflict_rule_base;
         CREATE RULE conflict_rule_insert AS ON INSERT TO conflict_rule_view
           DO ALSO INSERT INTO conflict_rule_log VALUES (NEW.id)",
    );
    let conflicted = exec(
        &engine,
        "INSERT INTO conflict_rule_view VALUES (1, 20) ON CONFLICT(id) DO NOTHING",
    );
    assert_eq!(conflicted.affected_rows, 0);
    assert_eq!(
        exec(&engine, "SELECT count(*) AS total FROM conflict_rule_log").rows[0]["total"],
        Value::Int(1)
    );

    exec(
        &engine,
        "CREATE TABLE lazy_select_base (id INTEGER);
         CREATE VIEW lazy_select_direct AS SELECT id FROM lazy_select_base;
         CREATE RULE lazy_select_direct_rule AS ON INSERT TO lazy_select_direct DO INSTEAD NOTHING",
    );
    let direct = exec(&engine, "INSERT INTO lazy_select_direct SELECT 1 / 0");
    assert_eq!(direct.affected_rows, 0);

    exec(
        &engine,
        "CREATE VIEW lazy_select_inner AS SELECT id FROM lazy_select_base;
         CREATE RULE lazy_select_inner_rule AS ON INSERT TO lazy_select_inner DO INSTEAD NOTHING;
         CREATE VIEW lazy_select_outer AS SELECT id FROM lazy_select_inner",
    );
    let nested = exec(&engine, "INSERT INTO lazy_select_outer SELECT 1 / 0");
    assert_eq!(nested.affected_rows, 0);
    assert!(exec(&engine, "SELECT * FROM lazy_select_base")
        .rows
        .is_empty());

    exec(
        &engine,
        "CREATE TABLE constant_rule_base (id INTEGER);
         CREATE TABLE constant_rule_log (marker INTEGER);
         CREATE VIEW constant_rule_view AS SELECT id FROM constant_rule_base;
         CREATE RULE constant_rule_insert AS ON INSERT TO constant_rule_view
           DO ALSO INSERT INTO constant_rule_log VALUES (1)",
    );
    exec(
        &engine,
        "INSERT INTO constant_rule_view VALUES (1), (2), (3)",
    );
    assert_eq!(
        exec(&engine, "SELECT count(*) AS total FROM constant_rule_log").rows[0]["total"],
        Value::Int(3)
    );
}

fn assert_suppressed_nested_and_direct_view_dml_is_lazy() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_lazy_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO nested_lazy_base VALUES (1, 10);
         CREATE VIEW nested_lazy_inner AS SELECT id, value FROM nested_lazy_base;
         CREATE RULE nested_lazy_inner_update AS ON UPDATE TO nested_lazy_inner DO INSTEAD NOTHING;
         CREATE VIEW nested_lazy_outer AS SELECT id, value FROM nested_lazy_inner",
    );
    let nested = exec(
        &engine,
        "UPDATE nested_lazy_outer SET value = 1 / 0 WHERE id = 1",
    );
    assert_eq!(nested.affected_rows, 0);
    assert_eq!(
        exec(&engine, "SELECT value FROM nested_lazy_base WHERE id = 1").rows[0]["value"],
        Value::Int(10)
    );

    exec(
        &engine,
        "CREATE TABLE nested_lazy_source (id INTEGER PRIMARY KEY);
         INSERT INTO nested_lazy_source VALUES (1)",
    );
    let nested_from = exec(
        &engine,
        "UPDATE nested_lazy_outer SET value = 1 / 0
         FROM nested_lazy_source AS source
         WHERE nested_lazy_outer.id = source.id",
    );
    assert_eq!(nested_from.affected_rows, 0);

    exec(
        &engine,
        "CREATE TABLE direct_lazy_base (id INTEGER PRIMARY KEY);
         INSERT INTO direct_lazy_base VALUES (1);
         CREATE VIEW direct_lazy_update AS
           SELECT id, 1 / (id - id) AS boom FROM direct_lazy_base;
         CREATE VIEW direct_lazy_delete AS
           SELECT id, 1 / (id - id) AS boom FROM direct_lazy_base;
         CREATE RULE direct_lazy_update_rule AS ON UPDATE TO direct_lazy_update DO INSTEAD NOTHING;
         CREATE RULE direct_lazy_delete_rule AS ON DELETE TO direct_lazy_delete DO INSTEAD NOTHING",
    );
    let updated = exec(
        &engine,
        "UPDATE direct_lazy_update SET id = id WHERE id = 1",
    );
    let deleted = exec(&engine, "DELETE FROM direct_lazy_delete WHERE id = 1");
    assert_eq!(updated.affected_rows, 0);
    assert_eq!(deleted.affected_rows, 0);
    assert_eq!(
        exec(&engine, "SELECT count(*) AS total FROM direct_lazy_base").rows[0]["total"],
        Value::Int(1)
    );
}

fn assert_rule_action_cardinality_matches_postgresql_18() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("rule-action-cardinality.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(
            &engine,
            "CREATE TABLE cardinality_update_base (id INTEGER PRIMARY KEY);
             INSERT INTO cardinality_update_base VALUES (1), (2), (3);
             CREATE TABLE cardinality_update_log (marker TEXT);
             CREATE RULE cardinality_update_rule AS ON UPDATE TO cardinality_update_base
               DO ALSO INSERT INTO cardinality_update_log VALUES ('updated');
             CREATE TABLE cardinality_update_source (id INTEGER);
             INSERT INTO cardinality_update_source VALUES (1), (2);
             CREATE TABLE cardinality_direct_base (id INTEGER PRIMARY KEY);
             INSERT INTO cardinality_direct_base VALUES (1), (2), (3);
             CREATE TABLE cardinality_direct_log (marker TEXT);
             CREATE VIEW cardinality_direct_view AS SELECT id FROM cardinality_direct_base;
             CREATE RULE cardinality_direct_rule AS ON UPDATE TO cardinality_direct_view
               DO INSTEAD INSERT INTO cardinality_direct_log VALUES ('updated');
             CREATE TABLE cardinality_delete_base (id INTEGER PRIMARY KEY);
             INSERT INTO cardinality_delete_base VALUES (1), (2), (3);
             CREATE TABLE cardinality_delete_log (marker TEXT);
             CREATE RULE cardinality_delete_rule AS ON DELETE TO cardinality_delete_base
               DO ALSO INSERT INTO cardinality_delete_log VALUES ('deleted');
             CREATE TABLE cardinality_delete_using_base (id INTEGER PRIMARY KEY);
             INSERT INTO cardinality_delete_using_base VALUES (1), (2);
             CREATE TABLE cardinality_delete_source (id INTEGER);
             INSERT INTO cardinality_delete_source VALUES (1), (1), (2), (3);
             CREATE TABLE cardinality_delete_using_log (marker TEXT);
             CREATE RULE cardinality_delete_using_rule AS ON DELETE TO cardinality_delete_using_base
               DO ALSO INSERT INTO cardinality_delete_using_log VALUES ('deleted');
             CREATE TABLE cardinality_direct_delete_base (id INTEGER PRIMARY KEY);
             INSERT INTO cardinality_direct_delete_base VALUES (1), (2);
             CREATE VIEW cardinality_direct_delete_view AS SELECT id FROM cardinality_direct_delete_base;
             CREATE TABLE cardinality_direct_delete_log (marker TEXT);
             CREATE RULE cardinality_direct_delete_rule AS ON DELETE TO cardinality_direct_delete_view
               DO INSTEAD INSERT INTO cardinality_direct_delete_log VALUES ('deleted')",
        );
    }
    let engine = Engine::open(&database).unwrap();
    assert_plain_update_rule_action_cardinality(&engine);
    assert_update_from_rule_action_cardinality(&engine);
    assert_plain_delete_rule_action_cardinality(&engine);
    assert_delete_using_rule_action_cardinality(&engine);
    assert_direct_view_delete_using_rule_action_cardinality(&engine);
}

fn assert_plain_update_rule_action_cardinality(engine: &Engine) {
    assert_eq!(
        exec(engine, "UPDATE cardinality_update_base SET id = id").affected_rows,
        3
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_update_log"
        )
        .rows[0]["total"],
        Value::Int(1)
    );
    exec(engine, "DELETE FROM cardinality_update_log");
}

fn assert_update_from_rule_action_cardinality(engine: &Engine) {
    assert_eq!(
        exec(
            engine,
            "UPDATE cardinality_update_base SET id = id WHERE false"
        )
        .affected_rows,
        0
    );
    assert!(exec(engine, "SELECT * FROM cardinality_update_log")
        .rows
        .is_empty());
    assert_eq!(
        exec(
            engine,
            "UPDATE cardinality_update_base SET id = id WHERE id > 0"
        )
        .affected_rows,
        3
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_update_log"
        )
        .rows[0]["total"],
        Value::Int(3)
    );
    exec(engine, "DELETE FROM cardinality_update_log");
    assert_eq!(
        exec(
            engine,
            "UPDATE cardinality_update_base AS target SET id = target.id
             FROM cardinality_update_source AS source"
        )
        .affected_rows,
        3
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_update_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
    exec(engine, "DELETE FROM cardinality_update_log");
    assert_eq!(
        exec(
            engine,
            "UPDATE cardinality_update_base AS target SET id = target.id
             FROM cardinality_update_source AS source WHERE target.id = 1"
        )
        .affected_rows,
        1
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_update_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
}

fn assert_plain_delete_rule_action_cardinality(engine: &Engine) {
    assert_eq!(
        exec(
            engine,
            "UPDATE cardinality_direct_view AS target SET id = target.id
             FROM cardinality_update_source AS source"
        )
        .affected_rows,
        0
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_direct_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
    exec(engine, "DELETE FROM cardinality_direct_log");
    assert_eq!(
        exec(
            engine,
            "UPDATE cardinality_direct_view AS target SET id = target.id
             FROM cardinality_update_source AS source WHERE target.id = 1"
        )
        .affected_rows,
        0
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_direct_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
    assert_eq!(
        exec(engine, "DELETE FROM cardinality_delete_base WHERE id > 1").affected_rows,
        2
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_delete_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
    exec(engine, "DELETE FROM cardinality_delete_log");
    assert_eq!(
        exec(engine, "DELETE FROM cardinality_delete_base WHERE false").affected_rows,
        0
    );
    assert!(exec(engine, "SELECT * FROM cardinality_delete_log")
        .rows
        .is_empty());
}

fn assert_delete_using_rule_action_cardinality(engine: &Engine) {
    assert_eq!(
        exec(
            engine,
            "DELETE FROM cardinality_delete_using_base AS target
             USING cardinality_delete_source AS source"
        )
        .affected_rows,
        2
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_delete_using_log"
        )
        .rows[0]["total"],
        Value::Int(4)
    );
    exec(
        engine,
        "INSERT INTO cardinality_delete_using_base VALUES (1), (2);
         DELETE FROM cardinality_delete_using_log",
    );
    assert_eq!(
        exec(
            engine,
            "DELETE FROM cardinality_delete_using_base AS target
             USING cardinality_delete_source AS source WHERE source.id > 1"
        )
        .affected_rows,
        2
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_delete_using_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
    exec(
        engine,
        "INSERT INTO cardinality_delete_using_base VALUES (1), (2);
         DELETE FROM cardinality_delete_using_log",
    );
    assert_eq!(
        exec(
            engine,
            "DELETE FROM cardinality_delete_using_base AS target
             USING cardinality_delete_source AS source WHERE target.id = source.id"
        )
        .affected_rows,
        2
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_delete_using_log"
        )
        .rows[0]["total"],
        Value::Int(3)
    );
    exec(engine, "DELETE FROM cardinality_delete_using_log");
    assert_eq!(
        exec(
            engine,
            "DELETE FROM cardinality_delete_using_base AS target
             USING cardinality_delete_source AS source WHERE source.id > 1"
        )
        .affected_rows,
        0
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_delete_using_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
}

fn assert_direct_view_delete_using_rule_action_cardinality(engine: &Engine) {
    for (sql, expected) in [
        (
            "DELETE FROM cardinality_direct_delete_view AS target USING cardinality_delete_source AS source",
            4,
        ),
        (
            "DELETE FROM cardinality_direct_delete_view AS target USING cardinality_delete_source AS source WHERE source.id > 1",
            2,
        ),
        (
            "DELETE FROM cardinality_direct_delete_view AS target USING cardinality_delete_source AS source WHERE target.id = source.id",
            3,
        ),
    ] {
        exec(engine, "DELETE FROM cardinality_direct_delete_log");
        assert_eq!(exec(engine, sql).affected_rows, 0);
        assert_eq!(
            exec(
                engine,
                "SELECT count(*) AS total FROM cardinality_direct_delete_log"
            )
            .rows[0]["total"],
            Value::Int(expected)
        );
    }
    exec(
        engine,
        "TRUNCATE cardinality_direct_delete_base;
         DELETE FROM cardinality_direct_delete_log",
    );
    assert_eq!(
        exec(
            engine,
            "DELETE FROM cardinality_direct_delete_view AS target
             USING cardinality_delete_source AS source WHERE source.id > 1"
        )
        .affected_rows,
        0
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_direct_delete_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
}

fn assert_rule_condition_case_projection_is_lazy() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE condition_lazy_base (id INTEGER PRIMARY KEY);
         INSERT INTO condition_lazy_base VALUES (1);
         CREATE TABLE condition_lazy_log (id INTEGER);
         CREATE VIEW condition_lazy_view AS
           SELECT id, 1 / (id - id) AS boom FROM condition_lazy_base;
         CREATE RULE condition_lazy_rule AS ON UPDATE TO condition_lazy_view
           WHERE CASE WHEN NEW.id = 1 THEN false ELSE NEW.boom > 0 END
           DO ALSO INSERT INTO condition_lazy_log VALUES (NEW.id)",
    );
    let updated = exec(
        &engine,
        "UPDATE condition_lazy_view SET id = id WHERE id = 1",
    );
    assert_eq!(updated.affected_rows, 1);
    assert!(exec(&engine, "SELECT * FROM condition_lazy_log")
        .rows
        .is_empty());
}

fn assert_correlated_source_only_names_remain_source_bound() {
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

fn assert_rule_updatability_catalog_flags() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE catalog_rule_base (id INTEGER);
         CREATE VIEW catalog_rule_insert AS SELECT DISTINCT id FROM catalog_rule_base;
         CREATE RULE catalog_rule_insert_rule AS ON INSERT TO catalog_rule_insert DO INSTEAD NOTHING;
         CREATE VIEW catalog_rule_update AS SELECT DISTINCT id FROM catalog_rule_base;
         CREATE RULE catalog_rule_update_rule AS ON UPDATE TO catalog_rule_update DO INSTEAD NOTHING;
         CREATE VIEW catalog_rule_all AS SELECT DISTINCT id FROM catalog_rule_base;
         CREATE RULE catalog_rule_all_insert AS ON INSERT TO catalog_rule_all DO INSTEAD NOTHING;
         CREATE RULE catalog_rule_all_update AS ON UPDATE TO catalog_rule_all DO INSTEAD NOTHING;
         CREATE RULE catalog_rule_all_delete AS ON DELETE TO catalog_rule_all DO INSTEAD NOTHING",
    );
    let views = exec(
        &engine,
        "SELECT table_name, is_updatable, is_insertable_into
         FROM information_schema.views
         WHERE table_name IN ('catalog_rule_insert', 'catalog_rule_update', 'catalog_rule_all')
         ORDER BY table_name",
    );
    assert_eq!(
        views.rows[0]["table_name"],
        Value::Str("catalog_rule_all".into())
    );
    assert_eq!(views.rows[0]["is_updatable"], Value::Str("YES".into()));
    assert_eq!(
        views.rows[0]["is_insertable_into"],
        Value::Str("YES".into())
    );
    assert_eq!(views.rows[1]["is_updatable"], Value::Str("NO".into()));
    assert_eq!(
        views.rows[1]["is_insertable_into"],
        Value::Str("YES".into())
    );
    assert_eq!(views.rows[2]["is_updatable"], Value::Str("NO".into()));
    assert_eq!(views.rows[2]["is_insertable_into"], Value::Str("NO".into()));
    let columns = exec(
        &engine,
        "SELECT table_name, is_updatable FROM information_schema.columns
         WHERE table_name IN ('catalog_rule_insert', 'catalog_rule_update', 'catalog_rule_all')
         ORDER BY table_name",
    );
    assert_eq!(columns.rows[0]["is_updatable"], Value::Str("YES".into()));
    assert_eq!(columns.rows[1]["is_updatable"], Value::Str("NO".into()));
    assert_eq!(columns.rows[2]["is_updatable"], Value::Str("NO".into()));
}

fn assert_check_option_error_order_and_mapped_duplicates() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE check_order_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO check_order_base VALUES (1, 1), (2, 2);
         CREATE VIEW check_order_view AS
           SELECT id, value FROM check_order_base WHERE value > 0 WITH CHECK OPTION",
    );
    let insert = engine
        .sql("INSERT INTO check_order_view VALUES (1, -1)", &[])
        .unwrap_err();
    assert_eq!(insert.sqlstate(), Some("23505"));
    let update = engine
        .sql(
            "UPDATE check_order_view SET id = 1, value = -1 WHERE id = 2",
            &[],
        )
        .unwrap_err();
    assert_eq!(update.sqlstate(), Some("23505"));

    exec(
        &engine,
        "CREATE TABLE duplicate_map_base (id INTEGER PRIMARY KEY, value INTEGER);
         CREATE VIEW duplicate_map_view (first_id, second_id, value) AS
           SELECT id, id, value FROM duplicate_map_base",
    );
    let duplicate = engine
        .sql(
            "INSERT INTO duplicate_map_view(first_id, second_id, value) VALUES (1, 1, 2)",
            &[],
        )
        .unwrap_err();
    assert_eq!(duplicate.sqlstate(), Some("42601"));
    exec(
        &engine,
        "INSERT INTO duplicate_map_view(first_id, value) VALUES (1, 2)
         ON CONFLICT(first_id, second_id) DO NOTHING",
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM duplicate_map_base WHERE id = 1").rows[0]["value"],
        Value::Int(2)
    );
}

fn assert_rule_suppression_defers_expressions_and_statement_triggers() {
    let engine = Engine::new();
    assert_suppressed_rules_defer_unused_expressions(&engine);
    assert_rules_can_consume_supplied_computed_columns(&engine);
    assert_multiple_instead_rules_use_the_final_action_count(&engine);
    assert_unconditional_instead_rules_suppress_view_triggers(&engine);
}

fn assert_suppressed_rules_defer_unused_expressions(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE lazy_rule_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO lazy_rule_base VALUES (1, 10);
         CREATE VIEW lazy_insert_rule_view AS SELECT id, value FROM lazy_rule_base;
         CREATE VIEW lazy_update_rule_view AS SELECT id, value FROM lazy_rule_base;
         CREATE RULE lazy_insert_rule AS ON INSERT TO lazy_insert_rule_view DO INSTEAD NOTHING;
         CREATE RULE lazy_update_rule AS ON UPDATE TO lazy_update_rule_view DO INSTEAD NOTHING",
    );
    let inserted = exec(
        engine,
        "INSERT INTO lazy_insert_rule_view VALUES (2, 1 / 0)",
    );
    assert_eq!(inserted.affected_rows, 0);
    let updated = exec(
        engine,
        "UPDATE lazy_update_rule_view SET value = 1 / 0 WHERE id = 1",
    );
    assert_eq!(updated.affected_rows, 0);
    assert_eq!(
        exec(engine, "SELECT value FROM lazy_rule_base WHERE id = 1").rows[0]["value"],
        Value::Int(10)
    );
}

fn assert_rules_can_consume_supplied_computed_columns(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE computed_rule_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO computed_rule_base VALUES (1, 2);
         CREATE TABLE computed_rule_log (event TEXT, id INTEGER, doubled INTEGER);
         CREATE VIEW computed_insert_rule_view AS
           SELECT id, value * 2 AS doubled FROM computed_rule_base;
         CREATE VIEW computed_update_rule_view AS
           SELECT id, value * 2 AS doubled FROM computed_rule_base;
         CREATE RULE computed_insert_rule AS ON INSERT TO computed_insert_rule_view
           DO INSTEAD INSERT INTO computed_rule_log VALUES ('insert', NEW.id, NEW.doubled);
         CREATE RULE computed_update_rule AS ON UPDATE TO computed_update_rule_view
           DO INSTEAD INSERT INTO computed_rule_log VALUES ('update', NEW.id, NEW.doubled)",
    );
    assert_eq!(
        exec(
            engine,
            "INSERT INTO computed_insert_rule_view VALUES (2, 8)"
        )
        .affected_rows,
        1
    );
    assert_eq!(
        exec(
            engine,
            "UPDATE computed_update_rule_view SET doubled = 12 WHERE id = 1"
        )
        .affected_rows,
        0
    );
    let computed = exec(
        engine,
        "SELECT event, id, doubled FROM computed_rule_log ORDER BY event",
    );
    assert_eq!(computed.rows[0]["event"], Value::Str("insert".into()));
    assert_eq!(computed.rows[0]["doubled"], Value::Int(8));
    assert_eq!(computed.rows[1]["event"], Value::Str("update".into()));
    assert_eq!(computed.rows[1]["doubled"], Value::Int(12));
}

fn assert_multiple_instead_rules_use_the_final_action_count(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE multi_rule_base (id INTEGER);
         CREATE TABLE multi_rule_first (id INTEGER);
         CREATE TABLE multi_rule_second (id INTEGER);
         CREATE VIEW multi_rule_view AS SELECT id FROM multi_rule_base;
         CREATE RULE multi_rule_a AS ON INSERT TO multi_rule_view
           DO INSTEAD INSERT INTO multi_rule_first VALUES (NEW.id);
         CREATE RULE multi_rule_b AS ON INSERT TO multi_rule_view
           DO INSTEAD INSERT INTO multi_rule_second VALUES (NEW.id)",
    );
    let multiple = exec(engine, "INSERT INTO multi_rule_view VALUES (1), (2)");
    assert_eq!(multiple.affected_rows, 2);
    assert_eq!(
        exec(engine, "SELECT count(*) AS total FROM multi_rule_first").rows[0]["total"],
        Value::Int(2)
    );
    assert_eq!(
        exec(engine, "SELECT count(*) AS total FROM multi_rule_second").rows[0]["total"],
        Value::Int(2)
    );
}

fn assert_unconditional_instead_rules_suppress_view_triggers(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE statement_rule_base (id INTEGER);
         CREATE TABLE statement_rule_log (
           seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
           event TEXT NOT NULL
         );
         CREATE VIEW statement_rule_view AS SELECT id FROM statement_rule_base;
         CREATE FUNCTION statement_rule_fn() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO statement_rule_log(event)
             VALUES (TG_WHEN || ':' || TG_LEVEL || ':' || TG_OP);
           RETURN NEW;
         END
         $$;
         CREATE TRIGGER statement_rule_before BEFORE INSERT ON statement_rule_view
           FOR EACH STATEMENT EXECUTE FUNCTION statement_rule_fn();
         CREATE TRIGGER statement_rule_row INSTEAD OF INSERT ON statement_rule_view
           FOR EACH ROW EXECUTE FUNCTION statement_rule_fn();
         CREATE TRIGGER statement_rule_after AFTER INSERT ON statement_rule_view
           FOR EACH STATEMENT EXECUTE FUNCTION statement_rule_fn();
         CREATE RULE statement_rule_instead AS ON INSERT TO statement_rule_view
           DO INSTEAD INSERT INTO statement_rule_log(event) VALUES ('rule')",
    );
    exec(engine, "INSERT INTO statement_rule_view VALUES (1)");
    let trigger_log = exec(engine, "SELECT event FROM statement_rule_log ORDER BY seq");
    assert_eq!(trigger_log.rows.len(), 1);
    assert_eq!(trigger_log.rows[0]["event"], Value::Str("rule".into()));
}

fn assert_automatic_view_subqueries_keep_the_complete_dml_scope() {
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

fn assert_rule_projection_defaults_returning_and_command_tags() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE default_rule_base (id INTEGER DEFAULT 5, value INTEGER);
         CREATE TABLE default_rule_log (id INTEGER);
         CREATE RULE default_rule AS ON INSERT TO default_rule_base
           DO ALSO INSERT INTO default_rule_log VALUES (NEW.id)",
    );
    exec(&engine, "INSERT INTO default_rule_base (value) VALUES (10)");
    assert_eq!(
        exec(&engine, "SELECT id FROM default_rule_log").rows[0]["id"],
        Value::Int(5)
    );
    exec(
        &engine,
        "CREATE TABLE identity_rule_base (
           id BIGINT GENERATED BY DEFAULT AS IDENTITY,
           value INTEGER
         );
         CREATE TABLE identity_rule_log (id BIGINT);
         CREATE RULE identity_rule AS ON INSERT TO identity_rule_base
           DO INSTEAD INSERT INTO identity_rule_log VALUES (NEW.id)",
    );
    let identity = exec(
        &engine,
        "INSERT INTO identity_rule_base (value) VALUES (10)",
    );
    assert_eq!(identity.affected_rows, 1);
    assert_eq!(
        exec(&engine, "SELECT id FROM identity_rule_log").rows[0]["id"],
        Value::Int(1)
    );

    exec(
        &engine,
        "CREATE TABLE conditional_projection_base (id INTEGER PRIMARY KEY);
         CREATE TABLE conditional_projection_log (value INTEGER);
         CREATE VIEW conditional_projection_view AS
           SELECT id, 1 / id AS danger FROM conditional_projection_base;
         CREATE RULE conditional_projection_rule AS ON INSERT TO conditional_projection_view
           WHERE NEW.id < 0
           DO ALSO INSERT INTO conditional_projection_log VALUES (NEW.danger)",
    );
    exec(
        &engine,
        "INSERT INTO conditional_projection_view (id) VALUES (0)",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS total FROM conditional_projection_log"
        )
        .rows[0]["total"],
        Value::Int(0)
    );

    exec(
        &engine,
        "CREATE TABLE provider_base (id INTEGER, value TEXT);
         CREATE TABLE provider_action (id INTEGER, value TEXT);
         CREATE VIEW provider_view (item_id, label) AS SELECT id, value FROM provider_base;
         CREATE RULE provider_rule AS ON INSERT TO provider_view
           DO INSTEAD INSERT INTO provider_action VALUES (NEW.item_id, NEW.label)
           RETURNING id, value",
    );
    let provider = exec(
        &engine,
        "INSERT INTO provider_view VALUES (1, 'one') RETURNING (SELECT label) AS got",
    );
    assert_eq!(provider.rows[0]["got"], Value::Str("one".into()));

    exec(
        &engine,
        "CREATE TABLE unrouted_tableoid_base (id INTEGER);
         CREATE TABLE unrouted_tableoid_log (was_null BOOLEAN);
         CREATE VIEW unrouted_tableoid_view AS
           SELECT id, tableoid AS physical_oid FROM unrouted_tableoid_base;
         CREATE RULE unrouted_tableoid_rule AS ON INSERT TO unrouted_tableoid_view
           DO INSTEAD INSERT INTO unrouted_tableoid_log VALUES (NEW.physical_oid IS NULL)",
    );
    let suppressed = exec(
        &engine,
        "INSERT INTO unrouted_tableoid_view (id) VALUES (1)",
    );
    assert_eq!(suppressed.affected_rows, 1);
    assert_eq!(
        exec(&engine, "SELECT was_null FROM unrouted_tableoid_log").rows[0]["was_null"],
        Value::Bool(true)
    );
}

fn assert_view_rules_precede_automatic_rewrite() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE view_rule_base (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
         CREATE TABLE view_rule_log (event TEXT NOT NULL, id INTEGER, value INTEGER);
         CREATE VIEW view_rule_instead AS SELECT id, value FROM view_rule_base;
         CREATE RULE view_insert_instead AS ON INSERT TO view_rule_instead
         DO INSTEAD INSERT INTO view_rule_log VALUES ('insert-instead', NEW.id, NEW.value)",
    );
    assert_view_insert_rules(&engine);
    assert_view_update_and_delete_rules(&engine);
    assert_view_rule_returning(&engine);
}

fn assert_nested_view_rules_run_at_each_rewrite_layer() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_rule_base (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
         CREATE TABLE nested_rule_log (
           seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
           event TEXT NOT NULL
         );
         CREATE VIEW nested_rule_inner AS SELECT id, value FROM nested_rule_base;
         CREATE VIEW nested_rule_outer AS SELECT id, value FROM nested_rule_inner;
         CREATE RULE nested_inner_insert AS ON INSERT TO nested_rule_inner
           DO ALSO INSERT INTO nested_rule_log(event) VALUES ('inner-insert');
         CREATE RULE nested_outer_insert AS ON INSERT TO nested_rule_outer
           DO ALSO INSERT INTO nested_rule_log(event) VALUES ('outer-insert');
         CREATE RULE nested_inner_update AS ON UPDATE TO nested_rule_inner
           DO ALSO INSERT INTO nested_rule_log(event) VALUES ('inner-update');
         CREATE RULE nested_outer_update AS ON UPDATE TO nested_rule_outer
           DO ALSO INSERT INTO nested_rule_log(event) VALUES ('outer-update');
         CREATE RULE nested_inner_delete AS ON DELETE TO nested_rule_inner
           DO ALSO INSERT INTO nested_rule_log(event) VALUES ('inner-delete');
         CREATE RULE nested_outer_delete AS ON DELETE TO nested_rule_outer
           DO ALSO INSERT INTO nested_rule_log(event) VALUES ('outer-delete')",
    );
    exec(&engine, "INSERT INTO nested_rule_outer VALUES (1, 10)");
    exec(
        &engine,
        "UPDATE nested_rule_outer SET value = 11 WHERE id = 1",
    );
    exec(&engine, "DELETE FROM nested_rule_outer WHERE id = 1");
    let log = exec(&engine, "SELECT event FROM nested_rule_log ORDER BY seq");
    assert_eq!(
        log.rows
            .iter()
            .map(|row| row["event"].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Str("inner-insert".into()),
            Value::Str("outer-insert".into()),
            Value::Str("outer-update".into()),
            Value::Str("inner-update".into()),
            Value::Str("outer-delete".into()),
            Value::Str("inner-delete".into()),
        ]
    );
    assert!(exec(&engine, "SELECT * FROM nested_rule_base")
        .rows
        .is_empty());
}

fn assert_rule_and_trigger_rewrite_order() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE rule_trigger_base (id INTEGER PRIMARY KEY);
         CREATE TABLE rule_trigger_log (
           seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
           event TEXT NOT NULL
         );
         CREATE FUNCTION rule_trigger_fn() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO rule_trigger_log(event) VALUES ('trigger');
           RETURN NEW;
         END
         $$;
         CREATE VIEW rule_trigger_also AS SELECT id FROM rule_trigger_base;
         CREATE TRIGGER rule_trigger_also_trigger INSTEAD OF INSERT ON rule_trigger_also
           FOR EACH ROW EXECUTE FUNCTION rule_trigger_fn();
         CREATE RULE rule_trigger_also_rule AS ON INSERT TO rule_trigger_also
           DO ALSO INSERT INTO rule_trigger_log(event) VALUES ('rule')",
    );
    exec(&engine, "INSERT INTO rule_trigger_also VALUES (1)");
    let log = exec(&engine, "SELECT event FROM rule_trigger_log ORDER BY seq");
    assert_eq!(
        log.rows
            .iter()
            .map(|row| row["event"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Str("trigger".into()), Value::Str("rule".into())]
    );

    exec(
        &engine,
        "TRUNCATE rule_trigger_log;
         CREATE VIEW rule_trigger_instead AS SELECT id FROM rule_trigger_base;
         CREATE TRIGGER rule_trigger_instead_trigger INSTEAD OF INSERT ON rule_trigger_instead
           FOR EACH ROW EXECUTE FUNCTION rule_trigger_fn();
         CREATE RULE rule_trigger_instead_rule AS ON INSERT TO rule_trigger_instead
           DO INSTEAD INSERT INTO rule_trigger_log(event) VALUES ('rule')",
    );
    exec(&engine, "INSERT INTO rule_trigger_instead VALUES (2)");
    let log = exec(&engine, "SELECT event FROM rule_trigger_log ORDER BY seq");
    assert_eq!(log.rows.len(), 1);
    assert_eq!(log.rows[0]["event"], Value::Str("rule".into()));
}

fn assert_update_delete_rule_and_trigger_rewrite_order() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE rule_trigger_ud_base (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
         INSERT INTO rule_trigger_ud_base VALUES (1, 10), (2, 20);
         CREATE TABLE rule_trigger_ud_log (
           seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
           event TEXT NOT NULL
         );
         CREATE FUNCTION rule_trigger_ud_fn() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO rule_trigger_ud_log(event) VALUES (lower(TG_OP) || '-trigger');
           IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
           RETURN NEW;
         END
         $$;
         CREATE VIEW rule_trigger_ud_view AS SELECT id, value FROM rule_trigger_ud_base;
         CREATE TRIGGER rule_trigger_ud INSTEAD OF UPDATE OR DELETE ON rule_trigger_ud_view
           FOR EACH ROW EXECUTE FUNCTION rule_trigger_ud_fn();
         CREATE RULE rule_trigger_update_also AS ON UPDATE TO rule_trigger_ud_view
           DO ALSO INSERT INTO rule_trigger_ud_log(event) VALUES ('update-rule');
         CREATE RULE rule_trigger_delete_also AS ON DELETE TO rule_trigger_ud_view
           DO ALSO INSERT INTO rule_trigger_ud_log(event) VALUES ('delete-rule')",
    );
    exec(
        &engine,
        "UPDATE rule_trigger_ud_view SET value = 11 WHERE id = 1",
    );
    exec(&engine, "DELETE FROM rule_trigger_ud_view WHERE id = 2");
    let log = exec(
        &engine,
        "SELECT event FROM rule_trigger_ud_log ORDER BY seq",
    );
    assert_eq!(
        log.rows
            .iter()
            .map(|row| row["event"].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Str("update-rule".into()),
            Value::Str("update-trigger".into()),
            Value::Str("delete-rule".into()),
            Value::Str("delete-trigger".into()),
        ]
    );

    exec(
        &engine,
        "TRUNCATE rule_trigger_ud_log;
         CREATE VIEW rule_trigger_ud_suppressed AS SELECT id, value FROM rule_trigger_ud_base;
         CREATE TRIGGER rule_trigger_ud_suppressed_trigger
           INSTEAD OF UPDATE OR DELETE ON rule_trigger_ud_suppressed
           FOR EACH ROW EXECUTE FUNCTION rule_trigger_ud_fn();
         CREATE RULE rule_trigger_update_instead AS ON UPDATE TO rule_trigger_ud_suppressed
           DO INSTEAD INSERT INTO rule_trigger_ud_log(event) VALUES ('update-rule');
         CREATE RULE rule_trigger_delete_instead AS ON DELETE TO rule_trigger_ud_suppressed
           DO INSTEAD INSERT INTO rule_trigger_ud_log(event) VALUES ('delete-rule')",
    );
    exec(
        &engine,
        "UPDATE rule_trigger_ud_suppressed SET value = 12 WHERE id = 1",
    );
    exec(
        &engine,
        "DELETE FROM rule_trigger_ud_suppressed WHERE id = 2",
    );
    let log = exec(
        &engine,
        "SELECT event FROM rule_trigger_ud_log ORDER BY seq",
    );
    assert_eq!(
        log.rows
            .iter()
            .map(|row| row["event"].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Str("update-rule".into()),
            Value::Str("delete-rule".into()),
        ]
    );
    let base = exec(
        &engine,
        "SELECT id, value FROM rule_trigger_ud_base ORDER BY id",
    );
    assert_eq!(base.rows[0]["value"], Value::Int(10));
    assert_eq!(base.rows[1]["value"], Value::Int(20));
}

fn assert_nested_insert_rule_suppression_and_order() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_suppression_base (id INTEGER PRIMARY KEY);
         CREATE TABLE nested_suppression_log (
           seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
           event TEXT NOT NULL
         );
         CREATE VIEW nested_suppression_inner AS SELECT id FROM nested_suppression_base;
         CREATE VIEW nested_suppression_outer AS SELECT id FROM nested_suppression_inner;
         CREATE RULE nested_suppression_inner_rule AS ON INSERT TO nested_suppression_inner
           DO ALSO INSERT INTO nested_suppression_log(event) VALUES ('inner');
         CREATE RULE nested_suppression_outer_rule AS ON INSERT TO nested_suppression_outer
           DO INSTEAD INSERT INTO nested_suppression_log(event) VALUES ('outer')",
    );
    exec(&engine, "INSERT INTO nested_suppression_outer VALUES (1)");
    let suppressed = exec(
        &engine,
        "SELECT event FROM nested_suppression_log ORDER BY seq",
    );
    assert_eq!(suppressed.rows.len(), 1);
    assert_eq!(suppressed.rows[0]["event"], Value::Str("outer".into()));
    assert!(exec(&engine, "SELECT * FROM nested_suppression_base")
        .rows
        .is_empty());

    exec(
        &engine,
        "CREATE TABLE layered_order_base (id INTEGER PRIMARY KEY);
         CREATE TABLE layered_order_log (
           seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
           event TEXT NOT NULL
         );
         CREATE VIEW layered_order_inner AS SELECT id FROM layered_order_base;
         CREATE VIEW layered_order_outer AS SELECT id FROM layered_order_inner;
         CREATE RULE layered_order_base_rule AS ON INSERT TO layered_order_base
           DO ALSO INSERT INTO layered_order_log(event) VALUES ('base');
         CREATE RULE layered_order_inner_rule AS ON INSERT TO layered_order_inner
           DO ALSO INSERT INTO layered_order_log(event) VALUES ('inner');
         CREATE RULE layered_order_outer_rule AS ON INSERT TO layered_order_outer
           DO ALSO INSERT INTO layered_order_log(event) VALUES ('outer')",
    );
    exec(&engine, "INSERT INTO layered_order_outer VALUES (1)");
    let ordered = exec(&engine, "SELECT event FROM layered_order_log ORDER BY seq");
    assert_eq!(
        ordered
            .rows
            .iter()
            .map(|row| row["event"].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Str("base".into()),
            Value::Str("inner".into()),
            Value::Str("outer".into()),
        ]
    );
}

fn assert_suppressed_rule_insert_does_not_prepare_base_row() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE suppressed_identity_base (
           id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
           payload TEXT DEFAULT 'base-default'
         );
         CREATE TABLE suppressed_identity_log (id BIGINT, payload TEXT);
         CREATE VIEW suppressed_identity_view AS SELECT id, payload FROM suppressed_identity_base;
         CREATE RULE suppressed_identity_rule AS ON INSERT TO suppressed_identity_view
           DO INSTEAD INSERT INTO suppressed_identity_log VALUES (NEW.id, NEW.payload)",
    );
    exec(
        &engine,
        "INSERT INTO suppressed_identity_view (payload) VALUES (DEFAULT)",
    );
    let rule_image = exec(&engine, "SELECT id, payload FROM suppressed_identity_log");
    assert_eq!(rule_image.rows[0]["id"], Value::Null);
    assert_eq!(rule_image.rows[0]["payload"], Value::Null);
    exec(
        &engine,
        "DROP RULE suppressed_identity_rule ON suppressed_identity_view",
    );
    let inserted = exec(
        &engine,
        "INSERT INTO suppressed_identity_view (payload) VALUES (DEFAULT) RETURNING id, payload",
    );
    assert_eq!(inserted.rows[0]["id"], Value::Int(1));
    assert_eq!(
        inserted.rows[0]["payload"],
        Value::Str("base-default".into())
    );

    exec(
        &engine,
        "CREATE TABLE suppressed_generated_base (
           id INTEGER PRIMARY KEY,
           boom INTEGER GENERATED ALWAYS AS (1 / (id - id)) VIRTUAL
         );
         CREATE TABLE suppressed_generated_log (id INTEGER);
         CREATE VIEW suppressed_generated_view AS SELECT id, boom FROM suppressed_generated_base;
         CREATE RULE suppressed_generated_rule AS ON INSERT TO suppressed_generated_view
           DO INSTEAD INSERT INTO suppressed_generated_log VALUES (NEW.id)",
    );
    exec(
        &engine,
        "INSERT INTO suppressed_generated_view (id) VALUES (1)",
    );
    let generated = exec(&engine, "SELECT id FROM suppressed_generated_log");
    assert_eq!(generated.rows[0]["id"], Value::Int(1));

    exec(
        &engine,
        "CREATE TABLE suppressed_partition_base (id INTEGER, value TEXT)
           PARTITION BY RANGE (id);
         CREATE TABLE suppressed_partition_low PARTITION OF suppressed_partition_base
           FOR VALUES FROM (0) TO (10);
         CREATE TABLE suppressed_partition_log (id INTEGER);
         CREATE VIEW suppressed_partition_view AS SELECT id, value FROM suppressed_partition_base;
         CREATE RULE suppressed_partition_rule AS ON INSERT TO suppressed_partition_view
           DO INSTEAD INSERT INTO suppressed_partition_log VALUES (NEW.id)",
    );
    exec(
        &engine,
        "INSERT INTO suppressed_partition_view VALUES (20, 'outside')",
    );
    let partitioned = exec(&engine, "SELECT id FROM suppressed_partition_log");
    assert_eq!(partitioned.rows[0]["id"], Value::Int(20));
}

fn assert_nested_rule_returning_provider_and_lazy_projection() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_returning_base (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
         CREATE TABLE nested_returning_action (id INTEGER, value INTEGER);
         CREATE TABLE nested_returning_log (id INTEGER);
         CREATE VIEW nested_returning_inner AS SELECT id, value FROM nested_returning_base;
         CREATE VIEW nested_returning_outer AS SELECT id, value FROM nested_returning_inner;
         CREATE RULE nested_returning_inner_rule AS ON INSERT TO nested_returning_inner
           DO INSTEAD INSERT INTO nested_returning_action VALUES (NEW.id, NEW.value)
           RETURNING id, value;
         CREATE RULE nested_returning_outer_rule AS ON INSERT TO nested_returning_outer
           DO ALSO INSERT INTO nested_returning_log VALUES (NEW.id)",
    );
    let returned = exec(
        &engine,
        "INSERT INTO nested_returning_outer VALUES (1, 10) RETURNING id, value",
    );
    assert_eq!(returned.rows.len(), 1);
    assert_eq!(returned.rows[0]["id"], Value::Int(1));
    assert_eq!(returned.rows[0]["value"], Value::Int(10));

    exec(
        &engine,
        "CREATE TABLE lazy_projection_base (id INTEGER PRIMARY KEY);
         CREATE TABLE lazy_projection_log (id INTEGER);
         CREATE VIEW lazy_projection_view AS
           SELECT id, 1 / (id - id) AS boom FROM lazy_projection_base;
         CREATE RULE lazy_projection_rule AS ON INSERT TO lazy_projection_view
           DO INSTEAD INSERT INTO lazy_projection_log VALUES (NEW.id)",
    );
    exec(&engine, "INSERT INTO lazy_projection_view (id) VALUES (1)");
    let logged = exec(&engine, "SELECT id FROM lazy_projection_log");
    assert_eq!(logged.rows[0]["id"], Value::Int(1));
}

fn assert_nested_conditional_instead_rule_rejects_statement() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_conditional_base (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
         INSERT INTO nested_conditional_base VALUES (1, 10), (2, 20);
         CREATE VIEW nested_conditional_inner AS SELECT id, value FROM nested_conditional_base;
         CREATE VIEW nested_conditional_outer AS SELECT id, value FROM nested_conditional_inner;
         CREATE RULE nested_conditional_rule AS ON UPDATE TO nested_conditional_inner
           WHERE OLD.id = 1 DO INSTEAD NOTHING",
    );
    let error = engine
        .sql("UPDATE nested_conditional_outer SET value = value + 1", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("55000"));
    let rows = exec(
        &engine,
        "SELECT id, value FROM nested_conditional_base ORDER BY id",
    );
    assert_eq!(rows.rows[0]["value"], Value::Int(10));
    assert_eq!(rows.rows[1]["value"], Value::Int(20));
}

fn assert_correlated_view_references_use_the_public_row_type() {
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

fn assert_unaliased_derived_source_keeps_source_only_names() {
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

fn assert_conditional_instead_rules_do_not_make_views_updatable() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE conditional_rule_base (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
         INSERT INTO conditional_rule_base VALUES (1, 10);
         CREATE VIEW conditional_insert_view AS SELECT id, value FROM conditional_rule_base;
         CREATE VIEW conditional_update_view AS SELECT id, value FROM conditional_rule_base;
         CREATE VIEW conditional_delete_view AS SELECT id, value FROM conditional_rule_base;
         CREATE RULE conditional_insert AS ON INSERT TO conditional_insert_view
           WHERE NEW.id > 0 DO INSTEAD NOTHING;
         CREATE RULE conditional_update AS ON UPDATE TO conditional_update_view
           WHERE OLD.id > 0 DO INSTEAD NOTHING;
         CREATE RULE conditional_delete AS ON DELETE TO conditional_delete_view
           WHERE OLD.id > 0 DO INSTEAD NOTHING",
    );
    for sql in [
        "INSERT INTO conditional_insert_view VALUES (2, 20)",
        "UPDATE conditional_update_view SET value = 11 WHERE id = 1",
        "DELETE FROM conditional_delete_view WHERE id = 1",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("55000"), "{sql}: {error}");
    }
    let rows = exec(
        &engine,
        "SELECT id, value FROM conditional_rule_base ORDER BY id",
    );
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0]["id"], Value::Int(1));
    assert_eq!(rows.rows[0]["value"], Value::Int(10));
}

fn assert_view_insert_rules(engine: &Engine) {
    exec(engine, "INSERT INTO view_rule_instead VALUES (1, 10)");
    assert!(exec(engine, "SELECT * FROM view_rule_base").rows.is_empty());
    let insert_log = exec(
        engine,
        "SELECT event, id, value FROM view_rule_log ORDER BY event",
    );
    assert_eq!(
        insert_log.rows[0]["event"],
        Value::Str("insert-instead".into())
    );
    assert_eq!(insert_log.rows[0]["id"], Value::Int(1));
    assert_eq!(insert_log.rows[0]["value"], Value::Int(10));

    exec(
        engine,
        "CREATE VIEW view_rule_also AS SELECT id, value FROM view_rule_base;
         CREATE RULE view_insert_also AS ON INSERT TO view_rule_also
         DO ALSO INSERT INTO view_rule_log VALUES ('insert-also', NEW.id, NEW.value)",
    );
    exec(engine, "INSERT INTO view_rule_also VALUES (2, 20)");
    let stored = exec(engine, "SELECT value FROM view_rule_base WHERE id = 2");
    assert_eq!(stored.rows[0]["value"], Value::Int(20));
    let also_log = exec(
        engine,
        "SELECT value FROM view_rule_log WHERE event = 'insert-also'",
    );
    assert_eq!(also_log.rows[0]["value"], Value::Int(20));
}

fn assert_view_update_and_delete_rules(engine: &Engine) {
    exec(
        engine,
        "INSERT INTO view_rule_base VALUES (3, 30);
         CREATE RULE view_update_instead AS ON UPDATE TO view_rule_instead
         DO INSTEAD INSERT INTO view_rule_log VALUES ('update-instead', OLD.id, NEW.value);
         CREATE RULE view_delete_instead AS ON DELETE TO view_rule_instead
         DO INSTEAD INSERT INTO view_rule_log VALUES ('delete-instead', OLD.id, OLD.value)",
    );
    let updated = exec(
        engine,
        "UPDATE view_rule_instead SET value = 31 WHERE id = 3",
    );
    assert_eq!(updated.affected_rows, 0);
    assert_eq!(
        exec(engine, "SELECT value FROM view_rule_base WHERE id = 3").rows[0]["value"],
        Value::Int(30)
    );
    assert_eq!(
        exec(
            engine,
            "SELECT value FROM view_rule_log WHERE event = 'update-instead'"
        )
        .rows[0]["value"],
        Value::Int(31)
    );
    let deleted = exec(engine, "DELETE FROM view_rule_instead WHERE id = 3");
    assert_eq!(deleted.affected_rows, 0);
    assert_eq!(
        exec(
            engine,
            "SELECT value FROM view_rule_log WHERE event = 'delete-instead'"
        )
        .rows[0]["value"],
        Value::Int(30)
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM view_rule_base WHERE id = 3"
        )
        .rows[0]["total"],
        Value::Int(1)
    );

    exec(
        engine,
        "CREATE RULE view_update_also AS ON UPDATE TO view_rule_also
         DO ALSO INSERT INTO view_rule_log VALUES ('update-also', OLD.id, NEW.value);
         CREATE RULE view_delete_also AS ON DELETE TO view_rule_also
         DO ALSO INSERT INTO view_rule_log VALUES ('delete-also', OLD.id, OLD.value)",
    );
    exec(engine, "UPDATE view_rule_also SET value = 21 WHERE id = 2");
    assert_eq!(
        exec(engine, "SELECT value FROM view_rule_base WHERE id = 2").rows[0]["value"],
        Value::Int(21)
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM view_rule_log WHERE event = 'update-also'"
        )
        .rows[0]["total"],
        Value::Int(1)
    );
    exec(engine, "DELETE FROM view_rule_also WHERE id = 2");
    assert!(exec(engine, "SELECT * FROM view_rule_base WHERE id = 2")
        .rows
        .is_empty());
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM view_rule_log WHERE event = 'delete-also'"
        )
        .rows[0]["total"],
        Value::Int(1)
    );
}

fn assert_view_rule_returning(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE view_rule_return_action (id INTEGER, value INTEGER);
         CREATE VIEW view_rule_returning AS SELECT id, value FROM view_rule_base;
         CREATE RULE view_insert_returning AS ON INSERT TO view_rule_returning
         DO INSTEAD INSERT INTO view_rule_return_action VALUES (NEW.id, NEW.value)
         RETURNING id, value",
    );
    let returned = exec(
        engine,
        "INSERT INTO view_rule_returning VALUES (4, 40) RETURNING id, value",
    );
    assert_eq!(returned.rows[0]["id"], Value::Int(4));
    assert_eq!(returned.rows[0]["value"], Value::Int(40));
    assert!(exec(engine, "SELECT * FROM view_rule_base WHERE id = 4")
        .rows
        .is_empty());
}

fn assert_view_rule_actions_with_duplicate_user_ids_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("automatic-view-rule-actions.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE persistent_rule_base (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
             CREATE TABLE persistent_rule_log (event TEXT NOT NULL, id INTEGER, value INTEGER);
             CREATE VIEW persistent_rule_view AS SELECT id, value FROM persistent_rule_base;
             CREATE RULE persistent_update_also AS ON UPDATE TO persistent_rule_view
             DO ALSO INSERT INTO persistent_rule_log VALUES ('update', OLD.id, NEW.value);
             CREATE RULE persistent_delete_also AS ON DELETE TO persistent_rule_view
             DO ALSO INSERT INTO persistent_rule_log VALUES ('delete', OLD.id, OLD.value);
             INSERT INTO persistent_rule_base VALUES (1, 10);
             UPDATE persistent_rule_view SET value = 11 WHERE id = 1",
        );
    }
    {
        let engine = Engine::open(&path).unwrap();
        exec(&engine, "DELETE FROM persistent_rule_view WHERE id = 1");
        let rows = exec(
            &engine,
            "SELECT event, id, value FROM persistent_rule_log ORDER BY event",
        );
        assert_eq!(rows.rows.len(), 2);
        assert_eq!(rows.rows[0]["event"], Value::Str("delete".into()));
        assert_eq!(rows.rows[0]["id"], Value::Int(1));
        assert_eq!(rows.rows[0]["value"], Value::Int(11));
        assert_eq!(rows.rows[1]["event"], Value::Str("update".into()));
        assert_eq!(rows.rows[1]["id"], Value::Int(1));
        assert_eq!(rows.rows[1]["value"], Value::Int(11));
    }
    let engine = Engine::open(&path).unwrap();
    assert_eq!(
        exec(&engine, "SELECT count(*) AS total FROM persistent_rule_log").rows[0]["total"],
        Value::Int(2)
    );
}

fn assert_simple_view_insert_defaults_upsert_and_computed_columns() {
    let engine = automatic_view_engine();

    let inserted = exec(
        &engine,
        "INSERT INTO automatic_items (item_id, label) VALUES (1, 'one'), (2, 'two')
         RETURNING item_id, label, visible, doubled",
    );
    assert_eq!(inserted.rows.len(), 2);
    assert_eq!(inserted.rows[0]["item_id"], Value::Int(1));
    assert_eq!(inserted.rows[0]["visible"], Value::Bool(true));
    assert_eq!(inserted.rows[0]["doubled"], Value::Int(14));

    let upserted = exec(
        &engine,
        "INSERT INTO automatic_items (item_id, label) VALUES (1, 'changed')
         ON CONFLICT (item_id) DO UPDATE SET label = excluded.label
         RETURNING item_id, label, doubled",
    );
    assert_eq!(upserted.rows[0]["item_id"], Value::Int(1));
    assert_eq!(upserted.rows[0]["label"], Value::Str("changed".into()));
    assert_eq!(upserted.rows[0]["doubled"], Value::Int(14));

    let ambiguous_conflict = engine
        .sql(
            "INSERT INTO automatic_items (item_id, label) VALUES (1, 'ambiguous')
             ON CONFLICT (item_id) DO UPDATE SET label = label",
            &[],
        )
        .unwrap_err();
    assert_eq!(ambiguous_conflict.sqlstate(), Some("42702"));

    let stored = exec(
        &engine,
        "SELECT id, value, visible, quantity FROM automatic_base ORDER BY id",
    );
    assert_eq!(stored.rows[0]["value"], Value::Str("changed".into()));
    assert_eq!(stored.rows[0]["quantity"], Value::Int(7));

    let computed_insert = engine
        .sql(
            "INSERT INTO automatic_items (item_id, label, doubled) VALUES (3, 'three', 6)",
            &[],
        )
        .unwrap_err();
    assert_eq!(computed_insert.sqlstate(), Some("0A000"));
    let computed_update = engine
        .sql(
            "UPDATE automatic_items SET doubled = 20 WHERE item_id = 1",
            &[],
        )
        .unwrap_err();
    assert_eq!(computed_update.sqlstate(), Some("0A000"));

    let implicit_values = exec(
        &engine,
        "INSERT INTO automatic_items VALUES (3, 'three')
         RETURNING item_id, label, visible, doubled",
    );
    assert_eq!(implicit_values.rows[0]["visible"], Value::Bool(true));
    assert_eq!(implicit_values.rows[0]["doubled"], Value::Int(14));
    exec(
        &engine,
        "CREATE TABLE automatic_input (id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
    );
    exec(&engine, "INSERT INTO automatic_input VALUES (4, 'four')");
    let implicit_select = exec(
        &engine,
        "INSERT INTO automatic_items SELECT id, label FROM automatic_input
         RETURNING item_id, label, visible, doubled",
    );
    assert_eq!(implicit_select.rows[0]["item_id"], Value::Int(4));
    assert_eq!(implicit_select.rows[0]["doubled"], Value::Int(14));
}

fn assert_update_from_delete_using_returning_and_visibility() {
    let engine = automatic_view_engine();
    exec(
        &engine,
        "INSERT INTO automatic_items (item_id, label) VALUES (1, 'one'), (2, 'two'), (3, 'three')",
    );
    exec(
        &engine,
        "CREATE TABLE automatic_source (id INTEGER PRIMARY KEY, next_value TEXT NOT NULL)",
    );
    exec(
        &engine,
        "INSERT INTO automatic_source VALUES (1, 'updated'), (3, 'removed')",
    );
    exec(
        &engine,
        "CREATE TABLE automatic_ambiguous_source (item_id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
    );
    exec(
        &engine,
        "INSERT INTO automatic_ambiguous_source VALUES (2, 'ambiguous')",
    );

    for sql in [
        "UPDATE automatic_items SET label = 'wrong'
         FROM automatic_ambiguous_source AS source
         WHERE item_id = source.item_id",
        "DELETE FROM automatic_items USING automatic_ambiguous_source AS source
         WHERE item_id = source.item_id",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42702"), "{sql}: {error}");
    }
    assert_eq!(
        exec(
            &engine,
            "SELECT label FROM automatic_items WHERE item_id = 2"
        )
        .rows[0]["label"],
        Value::Str("two".into())
    );

    let updated = exec(
        &engine,
        "UPDATE automatic_items AS target SET label = source.next_value || ':from'
         FROM automatic_source AS source
         WHERE target.item_id = source.id AND source.id = 1
         RETURNING target.item_id, source.next_value, label, doubled",
    );
    assert_eq!(updated.rows[0]["item_id"], Value::Int(1));
    assert_eq!(updated.rows[0]["next_value"], Value::Str("updated".into()));
    assert_eq!(updated.rows[0]["label"], Value::Str("updated:from".into()));
    assert_eq!(updated.rows[0]["doubled"], Value::Int(14));

    let hidden = exec(
        &engine,
        "UPDATE automatic_items SET visible = false WHERE item_id = 2
         RETURNING item_id, visible, doubled",
    );
    assert_eq!(hidden.rows[0]["visible"], Value::Bool(false));
    assert!(
        exec(&engine, "SELECT * FROM automatic_items WHERE item_id = 2")
            .rows
            .is_empty()
    );

    let deleted = exec(
        &engine,
        "DELETE FROM automatic_items AS target USING automatic_source AS source
         WHERE target.item_id = source.id AND source.id = 3
         RETURNING target.item_id, source.next_value, label, doubled",
    );
    assert_eq!(deleted.rows[0]["item_id"], Value::Int(3));
    assert_eq!(deleted.rows[0]["next_value"], Value::Str("removed".into()));
    assert_eq!(deleted.rows[0]["label"], Value::Str("three".into()));
    assert_eq!(deleted.rows[0]["doubled"], Value::Int(14));

    assert_update_from_delete_using_returning_stars(&engine);
}

fn assert_update_from_delete_using_returning_stars(engine: &Engine) {
    exec(
        engine,
        "INSERT INTO automatic_items (item_id, label) VALUES (5, 'five'), (6, 'six')",
    );
    exec(
        engine,
        "INSERT INTO automatic_source VALUES (5, 'star-update'), (6, 'star-delete')",
    );
    let update_star = exec(
        engine,
        "UPDATE automatic_items AS target SET label = source.next_value
         FROM automatic_source AS source
         WHERE target.item_id = source.id AND source.id = 5
         RETURNING *",
    );
    assert_eq!(
        update_star.columns,
        ["item_id", "label", "visible", "doubled", "id", "next_value"]
    );
    assert_eq!(update_star.value_at(0, 0), Some(&Value::Int(5)));
    assert_eq!(
        update_star.value_at(0, 4),
        Some(&Value::Int(5)),
        "RETURNING * must append the FROM row after the target view row"
    );
    assert_eq!(
        update_star.value_at(0, 5),
        Some(&Value::Str("star-update".into()))
    );
    let delete_star = exec(
        engine,
        "DELETE FROM automatic_items AS target USING automatic_source AS source
         WHERE target.item_id = source.id AND source.id = 6
         RETURNING *",
    );
    assert_eq!(
        delete_star.columns,
        ["item_id", "label", "visible", "doubled", "id", "next_value"]
    );
    assert_eq!(delete_star.value_at(0, 0), Some(&Value::Int(6)));
    assert_eq!(delete_star.value_at(0, 4), Some(&Value::Int(6)));
    assert_eq!(
        delete_star.value_at(0, 5),
        Some(&Value::Str("star-delete".into()))
    );
}

fn assert_view_row_type_is_the_dml_name_boundary() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE row_type_base (
            id INTEGER PRIMARY KEY,
            shown TEXT NOT NULL,
            secret TEXT NOT NULL
        );
         INSERT INTO row_type_base VALUES (1, 'shown', 'secret');
         CREATE VIEW row_type_view AS SELECT id, shown FROM row_type_base",
    );
    for sql in [
        "UPDATE row_type_view SET shown = secret WHERE id = 1",
        "UPDATE row_type_view SET shown = 'leaked' WHERE secret = 'secret'",
        "UPDATE row_type_view SET shown = 'leaked' WHERE id = 1 RETURNING secret",
        "INSERT INTO row_type_view (id, shown) VALUES (2, 'two') RETURNING secret",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42703"), "{sql}: {error}");
    }
    let unchanged = exec(&engine, "SELECT shown FROM row_type_base WHERE id = 1");
    assert_eq!(unchanged.rows[0]["shown"], Value::Str("shown".into()));
    assert!(exec(&engine, "SELECT * FROM row_type_base WHERE id = 2")
        .rows
        .is_empty());

    exec(
        &engine,
        "CREATE VIEW duplicate_mapping (first_id, second_id) AS
         SELECT id, id FROM row_type_base",
    );
    let duplicate = engine
        .sql(
            "UPDATE duplicate_mapping SET first_id = 2, second_id = 3",
            &[],
        )
        .unwrap_err();
    assert_eq!(duplicate.sqlstate(), Some("42601"));

    exec(
        &engine,
        "INSERT INTO row_type_base VALUES (2, 'shown-two', 'base-secret-two');
         CREATE TABLE row_type_source (id INTEGER PRIMARY KEY, secret TEXT NOT NULL);
         INSERT INTO row_type_source VALUES (1, 'source-secret'), (2, 'delete-secret')",
    );
    let updated = exec(
        &engine,
        "UPDATE row_type_view SET shown = secret
         FROM row_type_source AS source
         WHERE row_type_view.id = source.id AND source.id = 1
         RETURNING shown, secret AS source_secret",
    );
    assert_eq!(updated.rows[0]["shown"], Value::Str("source-secret".into()));
    assert_eq!(
        updated.rows[0]["source_secret"],
        Value::Str("source-secret".into())
    );
    let deleted = exec(
        &engine,
        "DELETE FROM row_type_view USING row_type_source AS source
         WHERE row_type_view.id = source.id AND secret = 'delete-secret'
         RETURNING row_type_view.id, secret AS source_secret",
    );
    assert_eq!(deleted.rows[0]["id"], Value::Int(2));
    assert_eq!(
        deleted.rows[0]["source_secret"],
        Value::Str("delete-secret".into())
    );
}

fn assert_source_old_and_new_aliases_remain_source_relations() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE alias_base (id INTEGER PRIMARY KEY, label TEXT NOT NULL);
         CREATE VIEW alias_view AS SELECT id, label FROM alias_base;
         CREATE TABLE alias_source (id INTEGER PRIMARY KEY, label TEXT NOT NULL);
         INSERT INTO alias_base VALUES (1, 'one'), (2, 'two');
         INSERT INTO alias_source VALUES (1, 'new-source'), (2, 'old-source')",
    );
    let updated = exec(
        &engine,
        "UPDATE alias_view AS target SET label = new.label
         FROM alias_source AS new
         WHERE target.id = new.id AND new.id = 1
         RETURNING target.id, new.label AS source_label, target.label",
    );
    assert_eq!(updated.rows[0]["id"], Value::Int(1));
    assert_eq!(
        updated.rows[0]["source_label"],
        Value::Str("new-source".into())
    );
    assert_eq!(updated.rows[0]["label"], Value::Str("new-source".into()));
    let deleted = exec(
        &engine,
        "DELETE FROM alias_view AS target USING alias_source AS old
         WHERE target.id = old.id AND old.id = 2
         RETURNING target.id, old.label AS source_label",
    );
    assert_eq!(deleted.rows[0]["id"], Value::Int(2));
    assert_eq!(
        deleted.rows[0]["source_label"],
        Value::Str("old-source".into())
    );
}

fn assert_base_triggers_replace_view_statement_triggers() {
    let engine = automatic_view_engine();
    exec(
        &engine,
        "CREATE TABLE automatic_log (
            seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
            entry TEXT NOT NULL
        )",
    );
    exec(
        &engine,
        "CREATE FUNCTION automatic_log_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO automatic_log(entry) VALUES
             (TG_TABLE_NAME || ':' || TG_WHEN || ':' || TG_LEVEL || ':' || TG_OP);
           RETURN NEW;
         END
         $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER view_before BEFORE INSERT ON automatic_items
         FOR EACH STATEMENT EXECUTE FUNCTION automatic_log_trigger()",
    );
    exec(
        &engine,
        "CREATE TRIGGER view_after AFTER INSERT ON automatic_items
         FOR EACH STATEMENT EXECUTE FUNCTION automatic_log_trigger()",
    );
    exec(
        &engine,
        "CREATE TRIGGER base_before BEFORE INSERT ON automatic_base
         FOR EACH STATEMENT EXECUTE FUNCTION automatic_log_trigger()",
    );
    exec(
        &engine,
        "CREATE TRIGGER base_after AFTER INSERT ON automatic_base
         FOR EACH STATEMENT EXECUTE FUNCTION automatic_log_trigger()",
    );

    exec(
        &engine,
        "INSERT INTO automatic_items (item_id, label) VALUES (1, 'one')",
    );
    let log = exec(&engine, "SELECT entry FROM automatic_log ORDER BY seq");
    assert_eq!(
        log.rows
            .iter()
            .map(|row| row["entry"].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Str("automatic_base:BEFORE:STATEMENT:INSERT".into()),
            Value::Str("automatic_base:AFTER:STATEMENT:INSERT".into()),
        ]
    );

    exec(
        &engine,
        "CREATE FUNCTION automatic_noop_view_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           RETURN NEW;
         END
         $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER automatic_instead_insert INSTEAD OF INSERT ON automatic_items
         FOR EACH ROW EXECUTE FUNCTION automatic_noop_view_trigger()",
    );
    exec(&engine, "SET session_replication_role = replica");
    let catalog = exec(
        &engine,
        "SELECT is_trigger_insertable_into FROM information_schema.views
         WHERE table_schema = 'public' AND table_name = 'automatic_items'",
    );
    assert_eq!(
        catalog.rows[0]["is_trigger_insertable_into"],
        Value::Str("YES".into())
    );
    let suppressed = exec(
        &engine,
        "INSERT INTO automatic_items (item_id, label) VALUES (99, 'suppressed') RETURNING *",
    );
    assert_eq!(suppressed.affected_rows, 0);
    assert!(suppressed.rows.is_empty());
    exec(&engine, "RESET session_replication_role");
    assert!(exec(&engine, "SELECT * FROM automatic_base WHERE id = 99")
        .rows
        .is_empty());
    assert_persistent_batch_can_create_check_trigger_after_routine_write();
}

fn assert_check_options_after_before_triggers(engine: &Engine) {
    exec(
        engine,
        "CREATE FUNCTION hide_checked_row() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.value LIKE 'hide%' THEN
             NEW.visible := false;
           END IF;
           RETURN NEW;
         END
         $$",
    );
    exec(
        engine,
        "CREATE TRIGGER hide_checked_row BEFORE INSERT OR UPDATE ON automatic_base
         FOR EACH ROW EXECUTE FUNCTION hide_checked_row()",
    );
    let before_insert = engine
        .sql(
            "INSERT INTO automatic_local (id, value) VALUES (11, 'hide-insert')",
            &[],
        )
        .unwrap_err();
    assert_eq!(before_insert.sqlstate(), Some("44000"));
    assert!(exec(engine, "SELECT * FROM automatic_base WHERE id = 11")
        .rows
        .is_empty());
    let multirow = engine
        .sql(
            "INSERT INTO automatic_local (id, value, visible)
             VALUES (11, 'eleven', true), (12, 'twelve', false)",
            &[],
        )
        .unwrap_err();
    assert_eq!(multirow.sqlstate(), Some("44000"));
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM automatic_base WHERE id IN (11, 12)"
        )
        .rows[0]["total"],
        Value::Int(0)
    );
    exec(
        engine,
        "INSERT INTO automatic_local (id, value) VALUES (11, 'eleven')",
    );
    let before_update = engine
        .sql(
            "UPDATE automatic_local SET value = 'hide-update' WHERE id = 11",
            &[],
        )
        .unwrap_err();
    assert_eq!(before_update.sqlstate(), Some("44000"));
    let conflict_update = engine
        .sql(
            "INSERT INTO automatic_local (id, value) VALUES (11, 'hide-conflict')
             ON CONFLICT (id) DO UPDATE SET value = excluded.value",
            &[],
        )
        .unwrap_err();
    assert_eq!(conflict_update.sqlstate(), Some("44000"));
    let unchanged = exec(
        engine,
        "SELECT value, visible FROM automatic_base WHERE id = 11",
    );
    assert_eq!(unchanged.rows[0]["value"], Value::Str("eleven".into()));
    assert_eq!(unchanged.rows[0]["visible"], Value::Bool(true));
}

fn assert_local_and_cascaded_check_options() {
    let engine = automatic_view_engine();
    exec(
        &engine,
        "CREATE VIEW automatic_local AS
         SELECT id, value, visible, quantity FROM automatic_base WHERE visible
         WITH LOCAL CHECK OPTION",
    );

    let rejected_insert = engine
        .sql(
            "INSERT INTO automatic_local (id, value, visible) VALUES (10, 'ten', false)",
            &[],
        )
        .unwrap_err();
    assert_eq!(rejected_insert.sqlstate(), Some("44000"));
    assert!(exec(&engine, "SELECT * FROM automatic_base WHERE id = 10")
        .rows
        .is_empty());

    exec(
        &engine,
        "INSERT INTO automatic_local (id, value) VALUES (10, 'ten')",
    );
    let rejected_update = engine
        .sql(
            "UPDATE automatic_local SET visible = false WHERE id = 10",
            &[],
        )
        .unwrap_err();
    assert_eq!(rejected_update.sqlstate(), Some("44000"));
    assert_eq!(
        exec(&engine, "SELECT visible FROM automatic_base WHERE id = 10").rows[0]["visible"],
        Value::Bool(true)
    );
    exec(
        &engine,
        "CREATE TABLE automatic_check_source (id INTEGER PRIMARY KEY)",
    );
    exec(&engine, "INSERT INTO automatic_check_source VALUES (10)");
    let rejected_update_from = engine
        .sql(
            "UPDATE automatic_local AS target SET visible = false
             FROM automatic_check_source AS source
             WHERE target.id = source.id AND source.id = 10",
            &[],
        )
        .unwrap_err();
    assert_eq!(rejected_update_from.sqlstate(), Some("44000"));
    assert_eq!(
        exec(&engine, "SELECT visible FROM automatic_base WHERE id = 10").rows[0]["visible"],
        Value::Bool(true)
    );

    assert_check_options_after_before_triggers(&engine);
    assert_nested_check_options(&engine);
}

fn assert_nested_check_options(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE nested_base (
            id INTEGER PRIMARY KEY,
            inner_ok BOOLEAN NOT NULL DEFAULT true,
            outer_ok BOOLEAN NOT NULL DEFAULT true,
            note TEXT NOT NULL DEFAULT 'defaulted'
        )",
    );
    exec(
        engine,
        "CREATE VIEW inner_open AS SELECT * FROM nested_base WHERE inner_ok",
    );
    exec(
        engine,
        "CREATE VIEW outer_local AS SELECT * FROM inner_open WHERE outer_ok WITH LOCAL CHECK OPTION",
    );
    exec(
        engine,
        "CREATE VIEW outer_cascaded AS SELECT * FROM inner_open WHERE outer_ok WITH CASCADED CHECK OPTION",
    );
    exec(
        engine,
        "INSERT INTO outer_local (id, inner_ok, outer_ok) VALUES (20, false, true)",
    );
    assert_eq!(
        exec(engine, "SELECT inner_ok FROM nested_base WHERE id = 20").rows[0]["inner_ok"],
        Value::Bool(false)
    );
    let cascaded = engine
        .sql(
            "INSERT INTO outer_cascaded (id, inner_ok, outer_ok) VALUES (21, false, true)",
            &[],
        )
        .unwrap_err();
    assert_eq!(cascaded.sqlstate(), Some("44000"));

    exec(
        engine,
        "CREATE VIEW ordered_inner AS
         SELECT * FROM nested_base WHERE inner_ok;
         CREATE VIEW ordered_outer AS
         SELECT * FROM ordered_inner WHERE 10 / id > 0 WITH CASCADED CHECK OPTION",
    );
    let ordered = engine
        .sql(
            "INSERT INTO ordered_outer (id, inner_ok, outer_ok) VALUES (0, false, true)",
            &[],
        )
        .unwrap_err();
    assert_eq!(
        ordered.sqlstate(),
        Some("44000"),
        "the inner view check must run before the outer division"
    );
}

fn assert_persistent_batch_can_create_check_trigger_after_routine_write() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("automatic-view-check-trigger.db");
    let engine = Engine::open(&path).unwrap();
    exec(
        &engine,
        "CREATE SCHEMA checked; CREATE TABLE checked.base_items (
            id INTEGER PRIMARY KEY,
            value TEXT NOT NULL,
            visible BOOLEAN NOT NULL DEFAULT true
        ); CREATE VIEW checked.items AS
            SELECT id, value, visible FROM checked.base_items WHERE visible
            WITH LOCAL CHECK OPTION",
    );
    exec(
        &engine,
        "SET search_path = checked, pg_catalog;
         CREATE FUNCTION hide_checked_row() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.value LIKE 'hide%' THEN
             NEW.visible := false;
           END IF;
           RETURN NEW;
         END
         $$;
         CREATE TRIGGER hide_checked_row BEFORE INSERT OR UPDATE ON base_items
         FOR EACH ROW EXECUTE FUNCTION hide_checked_row()",
    );

    let error = engine
        .sql(
            "INSERT INTO checked.items (id, value) VALUES (1, 'hide-insert')",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("44000"));
    assert!(exec(&engine, "SELECT * FROM checked.base_items")
        .rows
        .is_empty());
}

fn assert_nested_views_preserve_aliases_and_defaults() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_base (
            id INTEGER PRIMARY KEY,
            inner_ok BOOLEAN NOT NULL DEFAULT true,
            outer_ok BOOLEAN NOT NULL DEFAULT true,
            note TEXT NOT NULL DEFAULT 'defaulted'
        )",
    );
    exec(
        &engine,
        "CREATE VIEW inner_view AS SELECT id, inner_ok, outer_ok, note FROM nested_base WHERE inner_ok",
    );
    exec(
        &engine,
        "CREATE VIEW outer_view AS SELECT id, inner_ok, outer_ok, note FROM inner_view WHERE outer_ok WITH LOCAL CHECK OPTION",
    );
    exec(
        &engine,
        "CREATE VIEW nested_alias (renamed_id, renamed_note) AS SELECT id, note FROM outer_view",
    );

    let inserted = exec(
        &engine,
        "INSERT INTO nested_alias VALUES (24, 'twenty-four') RETURNING renamed_id, renamed_note",
    );
    assert_eq!(inserted.rows[0]["renamed_id"], Value::Int(24));
    let updated = exec(
        &engine,
        "UPDATE nested_alias SET renamed_note = renamed_note || ':updated'
         WHERE renamed_id = 24 RETURNING renamed_id, renamed_note",
    );
    assert_eq!(
        updated.rows[0]["renamed_note"],
        Value::Str("twenty-four:updated".into())
    );
    let stored = exec(
        &engine,
        "SELECT inner_ok, outer_ok, note FROM nested_base WHERE id = 24",
    );
    assert_eq!(stored.rows[0]["inner_ok"], Value::Bool(true));
    assert_eq!(stored.rows[0]["outer_ok"], Value::Bool(true));
    let deleted = exec(
        &engine,
        "DELETE FROM nested_alias WHERE renamed_id = 24 RETURNING renamed_id, renamed_note",
    );
    assert_eq!(deleted.rows[0]["renamed_id"], Value::Int(24));
    assert!(exec(&engine, "SELECT * FROM nested_base WHERE id = 24")
        .rows
        .is_empty());
}

fn assert_partition_tableoid_uses_the_physical_relation() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE partition_oid_base (id INTEGER, value TEXT NOT NULL) PARTITION BY RANGE (id);
         CREATE TABLE partition_oid_low PARTITION OF partition_oid_base FOR VALUES FROM (0) TO (10);
         CREATE TABLE partition_oid_high PARTITION OF partition_oid_base FOR VALUES FROM (10) TO (20);
         CREATE VIEW partition_oid_view AS
           SELECT id, value, tableoid AS physical_oid FROM partition_oid_base;
         INSERT INTO partition_oid_base VALUES (1, 'one')",
    );
    let updated = exec(
        &engine,
        "UPDATE partition_oid_view SET value = 'updated'
         WHERE physical_oid = 'partition_oid_low'::regclass
         RETURNING physical_oid = 'partition_oid_low'::regclass AS is_low",
    );
    assert_eq!(updated.rows[0]["is_low"], Value::Bool(true));

    let moved = exec(
        &engine,
        "UPDATE partition_oid_view SET id = 11 WHERE id = 1
         RETURNING WITH (OLD AS before, NEW AS after)
           before.physical_oid = 'partition_oid_low'::regclass AS old_is_low,
           after.physical_oid = 'partition_oid_high'::regclass AS new_is_high,
           physical_oid = 'partition_oid_high'::regclass AS current_is_high",
    );
    assert_eq!(moved.rows[0]["old_is_low"], Value::Bool(true));
    assert_eq!(moved.rows[0]["new_is_high"], Value::Bool(true));
    assert_eq!(moved.rows[0]["current_is_high"], Value::Bool(true));

    let deleted = exec(
        &engine,
        "DELETE FROM partition_oid_view
         WHERE physical_oid = 'partition_oid_high'::regclass
         RETURNING physical_oid = 'partition_oid_high'::regclass AS was_high",
    );
    assert_eq!(deleted.rows[0]["was_high"], Value::Bool(true));
}

fn assert_view_star_row_type_is_fixed_at_creation() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE fixed_star_base (id INTEGER PRIMARY KEY, value TEXT NOT NULL);
         CREATE VIEW fixed_star_view AS
           SELECT *, value || ':computed' AS computed FROM fixed_star_base;
         ALTER TABLE fixed_star_base ADD COLUMN added_later INTEGER NOT NULL DEFAULT 7",
    );
    let inserted = exec(
        &engine,
        "INSERT INTO fixed_star_view (id, value) VALUES (1, 'one') RETURNING *",
    );
    assert_eq!(inserted.columns, ["id", "value", "computed"]);
    assert_eq!(
        inserted.rows[0]["computed"],
        Value::Str("one:computed".into())
    );
    let selected = exec(&engine, "SELECT * FROM fixed_star_view");
    assert_eq!(selected.columns, ["id", "value", "computed"]);
    assert_eq!(
        selected.rows[0]["computed"],
        Value::Str("one:computed".into())
    );
    let base = exec(
        &engine,
        "SELECT added_later FROM fixed_star_base WHERE id = 1",
    );
    assert_eq!(base.rows[0]["added_later"], Value::Int(7));
}

fn non_updatable_view_engine() -> Engine {
    let engine = automatic_view_engine();
    exec(
        &engine,
        "CREATE TABLE automatic_source (id INTEGER PRIMARY KEY)",
    );
    exec(
        &engine,
        "CREATE VIEW distinct_view AS SELECT DISTINCT visible FROM automatic_base",
    );
    exec(
        &engine,
        "CREATE VIEW aggregate_view AS SELECT visible, count(*) AS total FROM automatic_base GROUP BY visible",
    );
    exec(
        &engine,
        "CREATE VIEW join_view AS
         SELECT base.id, base.value FROM automatic_base AS base
         JOIN automatic_source AS source ON source.id = base.id",
    );
    exec(
        &engine,
        "CREATE TABLE automatic_readonly_base (id INTEGER PRIMARY KEY)",
    );
    exec(&engine, "INSERT INTO automatic_readonly_base VALUES (1)");
    exec(
        &engine,
        "CREATE VIEW constant_only_view AS
         SELECT id + 1 AS computed_id FROM automatic_readonly_base",
    );
    exec(
        &engine,
        "CREATE VIEW xmin_view AS SELECT xmin FROM automatic_readonly_base",
    );
    exec(
        &engine,
        "CREATE VIEW set_returning_view AS
         SELECT id, generate_series(1, 2) AS generated FROM automatic_readonly_base",
    );
    exec(
        &engine,
        "CREATE MATERIALIZED VIEW automatic_materialized AS
         SELECT id, value FROM automatic_base",
    );
    engine
}

fn assert_non_updatable_views_and_catalog_flags() {
    let engine = non_updatable_view_engine();
    assert_non_updatable_view_dml(&engine);
    assert_non_updatable_view_catalog_flags(&engine);
}

fn assert_non_updatable_view_dml(engine: &Engine) {
    let invalid_check_option = engine
        .sql(
            "CREATE VIEW invalid_check_option AS
             SELECT DISTINCT visible FROM automatic_base WITH CHECK OPTION",
            &[],
        )
        .unwrap_err();
    assert_eq!(invalid_check_option.sqlstate(), Some("0A000"));
    exec(
        engine,
        "CREATE VIEW altered_invalid_check_option AS
         SELECT DISTINCT visible FROM automatic_base",
    );
    let altered_check_option = engine
        .sql(
            "ALTER VIEW altered_invalid_check_option SET (check_option = local)",
            &[],
        )
        .unwrap_err();
    assert_eq!(altered_check_option.sqlstate(), Some("0A000"));
    let check_option = exec(
        engine,
        "SELECT check_option FROM information_schema.views
         WHERE table_schema = 'public' AND table_name = 'altered_invalid_check_option'",
    );
    assert_eq!(
        check_option.rows[0]["check_option"],
        Value::Str("NONE".into())
    );
    for sql in [
        "INSERT INTO distinct_view VALUES (true)",
        "UPDATE join_view SET value = 'changed'",
        "DELETE FROM aggregate_view",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("55000"), "{sql}: {error}");
    }
    for sql in [
        "INSERT INTO constant_only_view VALUES (2)",
        "UPDATE constant_only_view SET computed_id = 3",
        "UPDATE set_returning_view SET id = 2",
        "INSERT INTO xmin_view VALUES ('1')",
        "UPDATE xmin_view SET xmin = '1'",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("55000"), "{sql}: {error}");
    }
    for sql in [
        "INSERT INTO constant_only_view (no_such_column) VALUES (2)",
        "UPDATE constant_only_view SET no_such_column = 3",
        "INSERT INTO join_view (no_such_column) VALUES (2)",
        "UPDATE join_view SET no_such_column = 3",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42703"), "{sql}: {error}");
    }
    for sql in [
        "UPDATE join_view AS target SET value = 'changed' WHERE target.secret = 'hidden'",
        "UPDATE join_view SET value = 'changed' RETURNING secret",
        "INSERT INTO join_view (id, value) VALUES (1, 'changed') RETURNING secret",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42703"), "{sql}: {error}");
    }

    exec(
        engine,
        "CREATE VIEW tableoid_view AS
         SELECT id, tableoid AS source_tableoid FROM automatic_readonly_base",
    );
    let expected_oid = exec(
        engine,
        "SELECT 'automatic_readonly_base'::regclass AS source_tableoid",
    );
    let returned_oid = exec(
        engine,
        "UPDATE tableoid_view SET id = id WHERE id = 1 RETURNING source_tableoid",
    );
    assert_eq!(
        returned_oid.rows[0]["source_tableoid"],
        expected_oid.rows[0]["source_tableoid"]
    );
    let deleted = exec(
        engine,
        "DELETE FROM constant_only_view WHERE computed_id = 2 RETURNING computed_id",
    );
    assert_eq!(deleted.rows[0]["computed_id"], Value::Int(2));
    assert!(exec(engine, "SELECT * FROM automatic_readonly_base")
        .rows
        .is_empty());
    for sql in [
        "INSERT INTO automatic_materialized VALUES (1, 'one')",
        "UPDATE automatic_materialized SET value = 'changed'",
        "DELETE FROM automatic_materialized",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42809"), "{sql}: {error}");
    }
}

fn assert_non_updatable_view_catalog_flags(engine: &Engine) {
    let views = exec(
        engine,
        "SELECT table_name, is_updatable, is_insertable_into, check_option
         FROM information_schema.views
         WHERE table_schema = 'public'
           AND table_name IN (
             'automatic_items', 'distinct_view', 'aggregate_view', 'join_view',
             'constant_only_view', 'set_returning_view', 'xmin_view'
           )
         ORDER BY table_name",
    );
    let flags = views
        .rows
        .iter()
        .map(|row| {
            (
                row["table_name"].clone(),
                row["is_updatable"].clone(),
                row["is_insertable_into"].clone(),
                row["check_option"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        flags,
        vec![
            (
                Value::Str("aggregate_view".into()),
                Value::Str("NO".into()),
                Value::Str("NO".into()),
                Value::Str("NONE".into()),
            ),
            (
                Value::Str("automatic_items".into()),
                Value::Str("YES".into()),
                Value::Str("YES".into()),
                Value::Str("NONE".into()),
            ),
            (
                Value::Str("constant_only_view".into()),
                Value::Str("NO".into()),
                Value::Str("NO".into()),
                Value::Str("NONE".into()),
            ),
            (
                Value::Str("distinct_view".into()),
                Value::Str("NO".into()),
                Value::Str("NO".into()),
                Value::Str("NONE".into()),
            ),
            (
                Value::Str("join_view".into()),
                Value::Str("NO".into()),
                Value::Str("NO".into()),
                Value::Str("NONE".into()),
            ),
            (
                Value::Str("set_returning_view".into()),
                Value::Str("NO".into()),
                Value::Str("NO".into()),
                Value::Str("NONE".into()),
            ),
            (
                Value::Str("xmin_view".into()),
                Value::Str("NO".into()),
                Value::Str("NO".into()),
                Value::Str("NONE".into()),
            ),
        ]
    );

    let columns = exec(
        engine,
        "SELECT column_name, is_updatable FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'automatic_items'
         ORDER BY ordinal_position",
    );
    assert_eq!(columns.rows[0]["is_updatable"], Value::Str("YES".into()));
    assert_eq!(columns.rows[1]["is_updatable"], Value::Str("YES".into()));
    assert_eq!(columns.rows[2]["is_updatable"], Value::Str("YES".into()));
    assert_eq!(columns.rows[3]["is_updatable"], Value::Str("NO".into()));
    let readonly_columns = exec(
        engine,
        "SELECT table_name, column_name, is_updatable
         FROM information_schema.columns
         WHERE table_schema = 'public'
           AND table_name IN ('constant_only_view', 'xmin_view')
         ORDER BY table_name, ordinal_position",
    );
    assert_eq!(readonly_columns.rows.len(), 2);
    assert!(readonly_columns
        .rows
        .iter()
        .all(|row| row["is_updatable"] == Value::Str("NO".into())));
}
