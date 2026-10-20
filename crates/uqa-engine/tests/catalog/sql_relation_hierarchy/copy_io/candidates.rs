//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command-snapshot identities across provider pages, partition movement and savepoint replay.

use super::{copy_engine, copy_to_string, exec, Engine};

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn mutation_identity_pages_preserve_fixed_changes_and_partition_targets(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("candidate-pages.db");
    let engine = copy_engine(provider, &path);
    exec(&engine, "CREATE TABLE candidate_pages(id INTEGER, value INTEGER) PARTITION BY RANGE(id);
        CREATE TABLE candidate_low PARTITION OF candidate_pages FOR VALUES FROM (MINVALUE) TO (256);
        CREATE TABLE candidate_high PARTITION OF candidate_pages FOR VALUES FROM (256) TO (MAXVALUE);
        INSERT INTO candidate_pages SELECT n, n FROM generate_series(1,257) AS s(n)");
    exec(&engine, "SET work_mem = '64kB'");
    exec(
        &engine,
        "BEGIN ISOLATION LEVEL REPEATABLE READ;
        SELECT count(*) FROM candidate_pages;
        INSERT INTO candidate_pages VALUES (0,0);
        DELETE FROM candidate_pages WHERE id=2;
        UPDATE candidate_pages SET id=302 WHERE id=3",
    );
    let updated = engine
        .sql("UPDATE candidate_pages SET value=value+1", &[])
        .unwrap();
    assert_eq!(updated.affected_rows, 257);
    verify(&engine);
    exec(&engine, "SAVEPOINT retained_candidates");
    let deleted = engine.sql("DELETE FROM candidate_pages", &[]).unwrap();
    assert_eq!(deleted.affected_rows, 257);
    assert_eq!(
        copy_to_string(
            &engine,
            "COPY (SELECT count(*) FROM candidate_pages) TO STDOUT"
        ),
        "0\n"
    );
    exec(&engine, "ROLLBACK TO retained_candidates; COMMIT");
    verify(&engine);
    drop(engine);
    if provider != 0 {
        verify(&copy_engine(provider, &path));
    }
}

fn verify(engine: &Engine) {
    assert_eq!(
        copy_to_string(
            engine,
            "COPY (SELECT count(*), sum(id), sum(value) FROM candidate_pages) TO STDOUT"
        ),
        "257\t33450\t33408\n"
    );
    assert_eq!(
        copy_to_string(
            engine,
            "COPY (SELECT count(*) FROM candidate_low) TO STDOUT"
        ),
        "254\n"
    );
    assert_eq!(
        copy_to_string(
            engine,
            "COPY (SELECT count(*) FROM candidate_high) TO STDOUT"
        ),
        "3\n"
    );
}
