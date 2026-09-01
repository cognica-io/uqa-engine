//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn sequence_cache_reservations_match_postgresql_boundaries_and_catalog_state() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SEQUENCE cached MINVALUE 1 MAXVALUE 20 CACHE 5;
             CREATE SEQUENCE partial MINVALUE 1 MAXVALUE 3 CACHE 5;
             CREATE SEQUENCE cycling MINVALUE 1 MAXVALUE 3 CACHE 5 CYCLE;
             CREATE SEQUENCE descending INCREMENT -2 MINVALUE -5 MAXVALUE 5 START 5 CACHE 4 CYCLE",
            &[],
        )
        .unwrap();

    assert_eq!(engine.nextval("cached").unwrap(), 1);
    let cached = engine.sequence_state("cached").unwrap().unwrap().1;
    assert_eq!(cached.current, 5);
    assert_eq!(cached.cache_size, 5);
    assert_eq!(engine.nextval("cached").unwrap(), 2);
    assert_eq!(
        engine.sequence_state("cached").unwrap().unwrap().1.current,
        5
    );
    let catalog = engine
        .sql(
            "SELECT cache_size, last_value FROM pg_sequences WHERE sequencename = 'cached'",
            &[],
        )
        .unwrap();
    assert_eq!(catalog.rows[0]["cache_size"], Value::Int(5));
    assert_eq!(catalog.rows[0]["last_value"], Value::Int(5));

    assert_eq!(engine.nextval("partial").unwrap(), 1);
    assert_eq!(
        engine.sequence_state("partial").unwrap().unwrap().1.current,
        3
    );
    assert_eq!(engine.nextval("partial").unwrap(), 2);
    assert_eq!(engine.nextval("partial").unwrap(), 3);
    assert_eq!(
        engine
            .sql("SELECT nextval('partial')", &[])
            .unwrap_err()
            .sqlstate(),
        Some("2200H")
    );

    for expected in [1, 2, 3, 1, 2, 3] {
        assert_eq!(engine.nextval("cycling").unwrap(), expected);
        assert_eq!(
            engine.sequence_state("cycling").unwrap().unwrap().1.current,
            3
        );
    }
    for (expected, reserved) in [(5, -1), (3, -1), (1, -1), (-1, -1), (-3, -5)] {
        assert_eq!(engine.nextval("descending").unwrap(), expected);
        assert_eq!(
            engine
                .sequence_state("descending")
                .unwrap()
                .unwrap()
                .1
                .current,
            reserved
        );
    }

    engine.sql("DISCARD SEQUENCES", &[]).unwrap();
    assert_eq!(engine.nextval("cached").unwrap(), 6);
    assert_eq!(
        engine.sequence_state("cached").unwrap().unwrap().1.current,
        10
    );
}

#[test]
fn sequence_cache_validation_is_atomic_and_failed_alter_retains_the_block() {
    let engine = Engine::new();
    for sql in [
        "CREATE SEQUENCE zero_cache CACHE 0",
        "CREATE SEQUENCE negative_cache CACHE -1",
    ] {
        assert_eq!(engine.sql(sql, &[]).unwrap_err().sqlstate(), Some("22023"));
    }
    engine
        .sql(
            "CREATE SEQUENCE maximum_cache CACHE 9223372036854775807;
             CREATE SEQUENCE retained_cache CACHE 5",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sequence_state("maximum_cache")
            .unwrap()
            .unwrap()
            .1
            .cache_size,
        i64::MAX
    );
    assert_eq!(engine.nextval("maximum_cache").unwrap(), 1);
    assert_eq!(
        engine
            .sequence_state("maximum_cache")
            .unwrap()
            .unwrap()
            .1
            .current,
        i64::MAX
    );
    assert_eq!(engine.nextval("maximum_cache").unwrap(), 2);
    assert_eq!(engine.nextval("retained_cache").unwrap(), 1);
    assert_eq!(
        engine
            .sql("ALTER SEQUENCE retained_cache CACHE 0", &[])
            .unwrap_err()
            .sqlstate(),
        Some("22023")
    );
    assert_eq!(engine.nextval("retained_cache").unwrap(), 2);
    assert_eq!(
        engine
            .sequence_state("retained_cache")
            .unwrap()
            .unwrap()
            .1
            .cache_size,
        5
    );
}

fn assert_sequence_cache_transaction_semantics(engine: &Engine) {
    engine
        .sql(
            "CREATE SEQUENCE transactional_cache CACHE 5;
             CREATE SEQUENCE transaction_first_cache CACHE 5",
            &[],
        )
        .unwrap();

    engine.begin().unwrap();
    assert_eq!(engine.nextval("transaction_first_cache").unwrap(), 1);
    assert_eq!(engine.nextval("transaction_first_cache").unwrap(), 2);
    engine.rollback().unwrap();
    assert_eq!(
        engine
            .sequence_state("transaction_first_cache")
            .unwrap()
            .unwrap()
            .1
            .current,
        5
    );
    assert_eq!(engine.nextval("transaction_first_cache").unwrap(), 3);

    assert_eq!(engine.nextval("transactional_cache").unwrap(), 1);
    assert_eq!(
        engine
            .sequence_state("transactional_cache")
            .unwrap()
            .unwrap()
            .1
            .current,
        5
    );

    engine.begin().unwrap();
    engine.savepoint("cached_values").unwrap();
    assert_eq!(engine.nextval("transactional_cache").unwrap(), 2);
    engine.rollback_to_savepoint("cached_values").unwrap();
    assert_eq!(engine.nextval("transactional_cache").unwrap(), 3);
    engine.rollback().unwrap();
    assert_eq!(engine.nextval("transactional_cache").unwrap(), 4);

    engine.sql("BEGIN READ ONLY", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT nextval('transactional_cache')", &[])
            .unwrap_err()
            .sqlstate(),
        Some("25006")
    );
    engine.rollback().unwrap();
    assert_eq!(engine.nextval("transactional_cache").unwrap(), 5);

    engine.begin().unwrap();
    engine
        .sql("ALTER SEQUENCE transactional_cache CACHE 2", &[])
        .unwrap();
    assert_eq!(engine.nextval("transactional_cache").unwrap(), 6);
    assert_eq!(
        engine
            .sequence_state("transactional_cache")
            .unwrap()
            .unwrap()
            .1
            .current,
        7
    );
    engine.rollback().unwrap();
    let restored = engine
        .sequence_state("transactional_cache")
        .unwrap()
        .unwrap()
        .1;
    assert_eq!((restored.current, restored.cache_size), (5, 5));
    assert_eq!(engine.currval("transactional_cache").unwrap(), 6);
    assert_eq!(engine.nextval("transactional_cache").unwrap(), 6);
    assert_eq!(
        engine
            .sequence_state("transactional_cache")
            .unwrap()
            .unwrap()
            .1
            .current,
        10
    );

    engine.setval("transactional_cache", 20).unwrap();
    assert_eq!(engine.nextval("transactional_cache").unwrap(), 21);
    assert_eq!(
        engine
            .sequence_state("transactional_cache")
            .unwrap()
            .unwrap()
            .1
            .current,
        25
    );
}

#[test]
fn sequence_cache_transaction_semantics_match_in_memory() {
    assert_sequence_cache_transaction_semantics(&Engine::new());
}

#[test]
fn sequence_cache_transaction_semantics_match_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    let engine =
        Engine::open(&directory.path().join("sequence-cache-transactions.sqlite")).unwrap();
    assert_sequence_cache_transaction_semantics(&engine);
}
