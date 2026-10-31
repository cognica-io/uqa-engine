//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::ProductionControl, CancellationToken};
use uqa_sql::plan::{RelationalPlan, SourcePlan};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_source_descriptors_admit_aliases_and_hold_the_last_schema_owner(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("source-descriptors.db"));
    engine.sql("CREATE TABLE source_items(id integer,v text); CREATE VIEW source_view AS SELECT id,v FROM source_items; CREATE MATERIALIZED VIEW source_mv AS SELECT id,v FROM source_items; CREATE SEQUENCE source_seq; CREATE SERVER source_server FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE source_remote(id integer,v text) SERVER source_server", &[]).unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(memory, resources.cancellation(), &invoking);
    let scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    let baseline = memory.used();
    for name in [
        "source_items",
        "source_view",
        "source_mv",
        "source_seq",
        "source_remote",
        "pg_catalog.pg_class",
        "information_schema.columns",
    ] {
        let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
            uqa_sql::compile(&format!("SELECT * FROM {name}"))
                .unwrap()
                .remove(0),
        ) else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("block")
        };
        let source = block.from.as_mut().unwrap();
        let SourcePlan::Table { column_aliases, .. } = source else {
            panic!("table")
        };
        column_aliases.push("label".repeat(1 << 18));
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(uqa_execution::query::binding::bind_source_plan_schema(
                &engine,
                source,
                &[],
                &scope,
                None,
                &control,
            ));
        });
        assert!(
            allocation.bytes_total < 256 << 10,
            "{provider:?} {name}: {allocation:?}"
        );
        assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
        assert_eq!(memory.used(), baseline + occupied.bytes());
        drop(occupied);
        for token in [resources.cancellation(), &invoking] {
            token.cancel();
            assert_eq!(
                uqa_execution::query::binding::bind_source_plan_schema(
                    &engine,
                    source,
                    &[],
                    &scope,
                    None,
                    &control
                )
                .unwrap_err()
                .sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), baseline);
            token.reset();
        }
        let schema = uqa_execution::query::binding::bind_source_plan_schema(
            &engine,
            source,
            &[],
            &scope,
            None,
            &control,
        )
        .unwrap();
        assert!(memory.used() > baseline + (1 << 20));
        let retained = schema.clone();
        let held = memory.used();
        drop(schema);
        assert_eq!(memory.used(), held);
        drop(retained);
        assert_eq!(memory.used(), baseline);
    }
    drop(scope);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

// Independently captured PostgreSQL 18.4 results are in tests/parity/pg18/source_schema_planning.expected.txt.
#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_source_descriptors_preserve_pg18_types_collations_and_namespace_order(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("source-descriptor-observations.db"));
    engine.sql("CREATE SCHEMA source_schema_owner; SET search_path=source_schema_owner,pg_catalog; CREATE TABLE items(id integer,v text COLLATE \"C\"); INSERT INTO items VALUES(1,'alpha'),(2,'beta'); CREATE VIEW items_view AS SELECT id,v FROM items; CREATE MATERIALIZED VIEW items_mv AS SELECT id,v FROM items; CREATE SEQUENCE items_seq", &[]).unwrap();
    for (sql, expected) in [
        ("SELECT pg_typeof(t)::text||':'||t::text AS result FROM items AS t(k,label) ORDER BY t.k", &["items:(1,alpha)","items:(2,beta)"][..]),
        ("SELECT pg_typeof(v)::text||':'||v::text AS result FROM items_view AS v(k,label) ORDER BY v.k", &["items_view:(1,alpha)","items_view:(2,beta)"][..]),
        ("SELECT pg_typeof(m)::text||':'||m::text AS result FROM items_mv AS m(k,label) ORDER BY m.k", &["items_mv:(1,alpha)","items_mv:(2,beta)"][..]),
        ("WITH c AS (SELECT id,v FROM items) SELECT c.k::text||':'||c.label||':'||pg_collation_for(c.label) AS result FROM c AS c(k,label) ORDER BY k", &["1:alpha:\"C\"","2:beta:\"C\""][..]),
        ("SELECT s::text||':'||pg_typeof(s.n)::text AS result FROM (SELECT 1::bigint AS id,'x'::text AS v) AS s(n,label)", &["(1,x):bigint"][..]),
        ("SELECT last_value::text||':'||is_called::text||':'||pg_typeof(last_value)::text AS result FROM items_seq", &["1:false:bigint"][..]),
        ("SELECT pg_typeof(relname)::text||':'||pg_typeof(relacl)::text AS result FROM pg_catalog.pg_class WHERE relname='items' AND relnamespace='source_schema_owner'::regnamespace", &["name:aclitem[]"][..]),
        ("SELECT pg_typeof(table_name)::text||':'||column_name::text AS result FROM information_schema.columns WHERE table_schema='source_schema_owner' AND table_name='items' ORDER BY ordinal_position", &["information_schema.sql_identifier:id","information_schema.sql_identifier:v"][..]),
    ] {
        let result = engine.sql(sql, &[]).unwrap();
        let actual = result.rows.iter().map(|row| match &row["result"] { Value::Str(value) => value.as_str(), value => panic!("{value:?}") }).collect::<Vec<_>>();
        assert_eq!(actual, expected, "{provider:?}: {sql}");
    }
    for (sql, state) in [
        ("SELECT * FROM items AS t(a,b,c)", "42P10"),
        ("SELECT t.missing FROM items AS t", "42703"),
    ] {
        assert_eq!(engine.sql(sql, &[]).unwrap_err().sqlstate(), Some(state));
        assert_eq!(
            engine
                .sql("SELECT count(*) AS n FROM items", &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(2)
        );
    }
    engine.close().unwrap();
}
