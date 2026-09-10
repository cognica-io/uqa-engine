//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 hierarchy catalog, system-column, and statistics parity.

use super::{exec, Engine, Value};

#[test]
fn partition_catalogs_bounds_and_deparsers_match_postgresql_18() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE catalog_range (id INTEGER, region TEXT) PARTITION BY RANGE (id)",
    );
    exec(
        &engine,
        "CREATE TABLE catalog_range_low PARTITION OF catalog_range FOR VALUES FROM (MINVALUE) TO (10)",
    );
    exec(
        &engine,
        "CREATE TABLE catalog_range_default PARTITION OF catalog_range DEFAULT",
    );
    exec(
        &engine,
        "CREATE TABLE catalog_hash (id INTEGER) PARTITION BY HASH (id)",
    );
    exec(
        &engine,
        "CREATE TABLE catalog_hash_r0 PARTITION OF catalog_hash FOR VALUES WITH (MODULUS 4, REMAINDER 0)",
    );

    let partitioned = engine
        .sql(
            "SELECT p.partstrat, p.partnatts, p.partattrs, p.partclass, p.partcollation, p.partexprs, pg_get_partkeydef(p.partrelid) AS keydef, d.relname AS default_name FROM pg_catalog.pg_partitioned_table AS p JOIN pg_catalog.pg_class AS c ON c.oid = p.partrelid LEFT JOIN pg_catalog.pg_class AS d ON d.oid = p.partdefid WHERE c.relname = 'catalog_range'",
            &[],
        )
        .unwrap();
    assert_eq!(partitioned.rows.len(), 1);
    let row = &partitioned.rows[0];
    assert_eq!(row["partstrat"], Value::Str("r".into()));
    assert_eq!(row["partnatts"], Value::Int(1));
    assert_eq!(row["partattrs"], Value::List(vec![Value::Int(1)]));
    assert_eq!(row["partclass"], Value::List(vec![Value::Int(1_978)]));
    assert_eq!(row["partcollation"], Value::List(vec![Value::Int(0)]));
    assert_eq!(row["partexprs"], Value::Null);
    assert_eq!(row["keydef"], Value::Str("RANGE (id)".into()));
    assert_eq!(
        row["default_name"],
        Value::Str("catalog_range_default".into())
    );

    let bound = engine
        .sql(
            "SELECT c.relpartbound, pg_get_expr(c.relpartbound, c.oid) AS expression, pg_typeof(c.relpartbound)::text AS bound_type FROM pg_catalog.pg_class AS c WHERE c.relname = 'catalog_hash_r0'",
            &[],
        )
        .unwrap();
    assert_eq!(bound.rows.len(), 1);
    assert_eq!(
        bound.rows[0]["relpartbound"],
        Value::Str("{PARTITIONBOUNDSPEC :strategy h :is_default false :modulus 4 :remainder 0 :listdatums <> :lowerdatums <> :upperdatums <> :location -1}".into())
    );
    assert_eq!(
        bound.rows[0]["expression"],
        Value::Str("FOR VALUES WITH (modulus 4, remainder 0)".into())
    );
    assert_eq!(
        bound.rows[0]["bound_type"],
        Value::Str("pg_node_tree".into())
    );

    let range_bound = engine
        .sql(
            "SELECT relpartbound, pg_get_expr(relpartbound, oid) AS expression FROM pg_catalog.pg_class WHERE relname = 'catalog_range_low'",
            &[],
        )
        .unwrap();
    assert_eq!(
        range_bound.rows[0]["relpartbound"],
        Value::Str("{PARTITIONBOUNDSPEC :strategy r :is_default false :modulus 0 :remainder 0 :listdatums <> :lowerdatums ({PARTITIONRANGEDATUM :kind -1 :value <> :location -1}) :upperdatums ({PARTITIONRANGEDATUM :kind 0 :value {CONST :consttype 23 :consttypmod -1 :constcollid 0 :constlen 4 :constbyval true :constisnull false :location -1 :constvalue 4 [ 10 0 0 0 0 0 0 0 ]} :location -1}) :location -1}".into())
    );
    assert_eq!(
        range_bound.rows[0]["expression"],
        Value::Str("FOR VALUES FROM (MINVALUE) TO (10)".into())
    );

    let absent = engine
        .sql(
            "SELECT pg_get_partkeydef(c.oid) AS keydef FROM pg_catalog.pg_class AS c WHERE c.relname = 'catalog_range_low'",
            &[],
        )
        .unwrap();
    assert_eq!(absent.rows[0]["keydef"], Value::Null);

    assert_partition_catalog_routines(&engine);

    exec(
        &engine,
        "CREATE VIEW partition_catalog_view AS SELECT partrelid FROM pg_catalog.pg_partitioned_table",
    );
    assert_eq!(
        engine
            .sql("SELECT partrelid FROM partition_catalog_view", &[])
            .unwrap()
            .rows
            .len(),
        2
    );
}

#[test]
fn constraint_relation_oids_preserve_quoted_dot_components() {
    let engine = Engine::new();
    exec(&engine, "CREATE SCHEMA \"catalog.dot\"");
    exec(
        &engine,
        "CREATE TABLE \"catalog.dot\".\"parent.dot\" (id INTEGER PRIMARY KEY)",
    );
    exec(
        &engine,
        "CREATE TABLE \"catalog.dot\".\"child.dot\" (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES \"catalog.dot\".\"parent.dot\"(id))",
    );
    let rows = engine
        .sql(
            "SELECT conrelid = child.oid AS constrained_matches, confrelid = parent.oid AS referenced_matches FROM pg_catalog.pg_constraint AS constraint_row JOIN pg_catalog.pg_class AS child ON child.oid = constraint_row.conrelid JOIN pg_catalog.pg_namespace AS child_ns ON child_ns.oid = child.relnamespace LEFT JOIN pg_catalog.pg_class AS parent ON parent.oid = constraint_row.confrelid WHERE child_ns.nspname = 'catalog.dot' AND child.relname = 'child.dot' AND constraint_row.contype = 'f'",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0]["constrained_matches"], Value::Bool(true));
    assert_eq!(rows.rows[0]["referenced_matches"], Value::Bool(true));
}

fn assert_partition_catalog_routines(engine: &Engine) {
    let routines = engine
        .sql(
            "SELECT oid, prosrc, proisstrict, provolatile, proparallel, proargtypes FROM pg_catalog.pg_proc WHERE oid IN (1716, 2509, 3352) ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 3);
    assert_eq!(routines.rows[0]["prosrc"], Value::Str("pg_get_expr".into()));
    assert_eq!(routines.rows[0]["proisstrict"], Value::Bool(true));
    assert_eq!(routines.rows[0]["provolatile"], Value::Str("s".into()));
    assert_eq!(routines.rows[0]["proparallel"], Value::Str("s".into()));
    assert_eq!(
        routines.rows[0]["proargtypes"],
        Value::List(vec![Value::Int(194), Value::Int(26)])
    );
    assert_eq!(
        routines.rows[1]["proargtypes"],
        Value::List(vec![Value::Int(194), Value::Int(26), Value::Int(16)])
    );
    assert_eq!(
        routines.rows[2]["prosrc"],
        Value::Str("pg_get_partkeydef".into())
    );
}

#[test]
fn inherited_column_provenance_and_partitioned_index_identity_are_catalogued() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE provenance_left (left_value INTEGER, common INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE provenance_right (right_value INTEGER, common INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE provenance_child (common INTEGER, local_value INTEGER) INHERITS (provenance_left, provenance_right)",
    );
    let attributes = engine
        .sql(
            "SELECT a.attname, a.attislocal, a.attinhcount FROM pg_catalog.pg_attribute AS a JOIN pg_catalog.pg_class AS c ON c.oid = a.attrelid WHERE c.relname = 'provenance_child' ORDER BY a.attnum",
            &[],
        )
        .unwrap();
    assert_eq!(attributes.rows.len(), 4);
    assert_eq!(
        attributes.rows[0]["attname"],
        Value::Str("left_value".into())
    );
    assert_eq!(attributes.rows[0]["attislocal"], Value::Bool(false));
    assert_eq!(attributes.rows[0]["attinhcount"], Value::Int(1));
    assert_eq!(attributes.rows[1]["attname"], Value::Str("common".into()));
    assert_eq!(attributes.rows[1]["attislocal"], Value::Bool(true));
    assert_eq!(attributes.rows[1]["attinhcount"], Value::Int(2));
    assert_eq!(
        attributes.rows[2]["attname"],
        Value::Str("right_value".into())
    );
    assert_eq!(attributes.rows[2]["attislocal"], Value::Bool(false));
    assert_eq!(
        attributes.rows[3]["attname"],
        Value::Str("local_value".into())
    );
    assert_eq!(attributes.rows[3]["attislocal"], Value::Bool(true));

    exec(
        &engine,
        "CREATE TABLE indexed_parent (id INTEGER) PARTITION BY RANGE (id)",
    );
    exec(
        &engine,
        "CREATE TABLE indexed_child PARTITION OF indexed_parent FOR VALUES FROM (0) TO (10)",
    );
    exec(
        &engine,
        "CREATE INDEX indexed_parent_id_idx ON indexed_parent (id)",
    );
    let index = engine
        .sql(
            "SELECT c.relam, c.relkind, c.relnatts, c.relispartition, c.relhassubclass, i.indrelid = t.oid AS table_matches, x.indexdef FROM pg_catalog.pg_class AS c JOIN pg_catalog.pg_index AS i ON i.indexrelid = c.oid JOIN pg_catalog.pg_class AS t ON t.oid = i.indrelid JOIN pg_catalog.pg_indexes AS x ON x.indexname = c.relname WHERE c.relname = 'indexed_parent_id_idx'",
            &[],
        )
        .unwrap();
    assert_eq!(index.rows.len(), 1);
    assert_eq!(index.rows[0]["relam"], Value::Int(403));
    assert_eq!(index.rows[0]["relkind"], Value::Str("I".into()));
    assert_eq!(index.rows[0]["relnatts"], Value::Int(1));
    assert_eq!(index.rows[0]["relispartition"], Value::Bool(false));
    assert_eq!(index.rows[0]["relhassubclass"], Value::Bool(true));
    assert_eq!(index.rows[0]["table_matches"], Value::Bool(true));
    assert_eq!(
        index.rows[0]["indexdef"],
        Value::Str(
            "CREATE INDEX indexed_parent_id_idx ON ONLY public.indexed_parent USING btree (id)"
                .into()
        )
    );
    let child_index = engine
        .sql(
            "SELECT child.relname, child.relkind, child.relispartition FROM pg_catalog.pg_inherits AS i JOIN pg_catalog.pg_class AS child ON child.oid = i.inhrelid JOIN pg_catalog.pg_class AS parent ON parent.oid = i.inhparent WHERE parent.relname = 'indexed_parent_id_idx'",
            &[],
        )
        .unwrap();
    assert_eq!(child_index.rows.len(), 1);
    assert_eq!(
        child_index.rows[0]["relname"],
        Value::Str("indexed_child_id_idx".into())
    );
    assert_eq!(child_index.rows[0]["relkind"], Value::Str("i".into()));
    assert_eq!(child_index.rows[0]["relispartition"], Value::Bool(true));
}

#[test]
fn tableoid_and_statistics_follow_physical_hierarchy_members() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE statistic_parent (id INTEGER) PARTITION BY RANGE (id)",
    );
    exec(
        &engine,
        "CREATE TABLE statistic_low PARTITION OF statistic_parent FOR VALUES FROM (MINVALUE) TO (10)",
    );
    exec(
        &engine,
        "CREATE TABLE statistic_high PARTITION OF statistic_parent FOR VALUES FROM (10) TO (MAXVALUE)",
    );
    exec(
        &engine,
        "INSERT INTO statistic_parent VALUES (1), (11), (12)",
    );
    let rows = engine
        .sql(
            "SELECT p.id, c.relname FROM statistic_parent AS p JOIN pg_catalog.pg_class AS c ON c.oid = p.tableoid ORDER BY p.id",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 3);
    assert_eq!(rows.rows[0]["relname"], Value::Str("statistic_low".into()));
    assert_eq!(rows.rows[1]["relname"], Value::Str("statistic_high".into()));
    assert_eq!(rows.rows[2]["relname"], Value::Str("statistic_high".into()));
    assert!(!engine
        .sql("SELECT * FROM statistic_parent ORDER BY id", &[])
        .unwrap()
        .rows[0]
        .contains_key("tableoid"));

    let stats = engine.column_stats("statistic_parent").unwrap();
    assert_eq!(stats["id"].row_count, 3);
    exec(&engine, "INSERT INTO statistic_low VALUES (2)");
    let refreshed = engine.column_stats("statistic_parent").unwrap();
    assert_eq!(refreshed["id"].row_count, 4);
    assert_eq!(refreshed["id"].distinct_count, 4);

    let class = engine
        .sql(
            "SELECT reltuples FROM pg_catalog.pg_class WHERE relname = 'statistic_parent'",
            &[],
        )
        .unwrap();
    assert_eq!(class.rows[0]["reltuples"], Value::Float(4.0));
}

#[test]
fn hierarchy_catalog_provenance_and_bounds_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hierarchy-catalog.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE durable_parent (id INTEGER, common INTEGER)",
        );
        exec(
            &engine,
            "CREATE TABLE durable_child (common INTEGER) INHERITS (durable_parent)",
        );
        exec(
            &engine,
            "CREATE TABLE durable_parts (id INTEGER) PARTITION BY HASH (id)",
        );
        exec(
            &engine,
            "CREATE TABLE durable_parts_r0 PARTITION OF durable_parts FOR VALUES WITH (MODULUS 2, REMAINDER 0)",
        );
    }
    let reopened = Engine::open(&path).unwrap();
    let provenance = reopened
        .sql(
            "SELECT a.attislocal, a.attinhcount FROM pg_catalog.pg_attribute AS a JOIN pg_catalog.pg_class AS c ON c.oid = a.attrelid WHERE c.relname = 'durable_child' AND a.attname = 'common'",
            &[],
        )
        .unwrap();
    assert_eq!(provenance.rows[0]["attislocal"], Value::Bool(true));
    assert_eq!(provenance.rows[0]["attinhcount"], Value::Int(1));
    let bound = reopened
        .sql(
            "SELECT pg_get_expr(relpartbound, oid) AS expression FROM pg_catalog.pg_class WHERE relname = 'durable_parts_r0'",
            &[],
        )
        .unwrap();
    assert_eq!(
        bound.rows[0]["expression"],
        Value::Str("FOR VALUES WITH (modulus 2, remainder 0)".into())
    );
}
