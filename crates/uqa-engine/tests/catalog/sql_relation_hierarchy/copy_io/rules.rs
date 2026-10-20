//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn rule_retained_inputs_preserve_composite_reads_multiplicity_and_rollback(
    #[case] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rule-input.db");
    let engine = copy_engine(provider, &path);
    exec(&engine, "CREATE TYPE retained_pair AS (a INTEGER);
        CREATE TABLE retained_events(id INTEGER PRIMARY KEY, obj retained_pair, body TEXT);
        CREATE TABLE retained_log(id INTEGER, copy INTEGER, old_a INTEGER, new_b TEXT, size INTEGER, whole JSONB);
        INSERT INTO retained_events SELECT n, ROW(n)::retained_pair, repeat('x',4096) FROM generate_series(1,8) AS s(n);
        ALTER TYPE retained_pair ADD ATTRIBUTE b TEXT;
        CREATE RULE capture AS ON UPDATE TO retained_events DO ALSO
          INSERT INTO retained_log SELECT OLD.id, d.n, (OLD.obj).a, (NEW.obj).b, length(NEW.body), to_jsonb(OLD)
          FROM (VALUES (1), (2)) AS d(n)");
    engine.set_query_memory_limit(Some(1024));
    exec(&engine, "UPDATE retained_events SET body=body || '!'");
    assert_eq!(copy_to_string(&engine, "COPY (SELECT count(*), sum(id), sum(copy), sum(old_a), count(new_b), sum(size), bool_and(whole->'obj' = jsonb_build_object('a',id,'b',NULL)) FROM retained_log) TO STDOUT WITH (FORMAT csv)"), "16,72,24,72,0,65552,t\n");
    exec(&engine, "UPDATE retained_events SET body=body WHERE id < 0");
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (SELECT count(*) FROM retained_log) TO STDOUT"
        ),
        "16\n"
    );
    exec(
        &engine,
        "BEGIN; UPDATE retained_events SET body=body || '?'",
    );
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (SELECT count(*), max(size) FROM retained_log) TO STDOUT WITH (FORMAT csv)"
        ),
        "32,4098\n"
    );
    exec(&engine, "ROLLBACK");
    let verify = |engine: &Engine| {
        assert_eq!(copy_to_string(engine, "COPY (SELECT count(*), min(length(body)), max(length(body)) FROM retained_log CROSS JOIN retained_events) TO STDOUT WITH (FORMAT csv)"), "128,4097,4097\n");
    };
    verify(&engine);
    drop(engine);
    if provider != 0 {
        let reopened = copy_engine(provider, &path);
        verify(&reopened);
    }
}
