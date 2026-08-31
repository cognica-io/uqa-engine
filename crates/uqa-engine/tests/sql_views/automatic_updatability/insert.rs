//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

pub(super) fn assert_only_partition_view_insert_routes_to_a_partition() {
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

pub(super) fn assert_nested_rule_backed_computed_columns() {
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

pub(super) fn assert_nested_nonautomatic_rule_backed_view_executes() {
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

pub(super) fn assert_nonautomatic_rule_boundary_preserves_outer_layers() {
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

pub(super) fn assert_rule_backed_view_inputs_are_evaluated_lazily() {
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

pub(super) fn assert_nested_instead_rules_stop_lower_rewrite_layers() {
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

pub(super) fn assert_rule_insert_images_conflicts_and_lazy_sources() {
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

pub(super) fn assert_suppressed_nested_and_direct_view_dml_is_lazy() {
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

pub(super) fn assert_rule_condition_case_projection_is_lazy() {
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

pub(super) fn assert_nested_insert_rule_suppression_and_order() {
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

pub(super) fn assert_suppressed_rule_insert_does_not_prepare_base_row() {
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

pub(super) fn assert_simple_view_insert_defaults_upsert_and_computed_columns() {
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

pub(super) fn assert_nested_views_preserve_aliases_and_defaults() {
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

pub(super) fn assert_partition_tableoid_uses_the_physical_relation() {
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
