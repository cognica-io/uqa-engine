//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A row at an identity its table never used is written without reading earlier records. An identity that was used, in this transaction or before it, is still met where it left its records.

use super::*;

fn keyed(engine: &Engine) -> Vec<String> {
    rows(
        engine,
        "SELECT id, body, tag FROM keyed ORDER BY id",
        &["id", "body", "tag"],
    )
}

/// The identities the index on `tag` answers for one value.
fn tagged(engine: &Engine, tag: i64) -> Vec<String> {
    rows(
        engine,
        &format!("SELECT id FROM keyed WHERE tag = {tag} ORDER BY id"),
        &["id"],
    )
}

/// A table keyed by its identity with an index and a BLOB column, holding the identities 1 to 9, which were above its watermark when they were inserted.
fn keyed_table(layout: Layout, path: &Path) -> Engine {
    let engine = open(layout, path);
    run(
        &engine,
        "CREATE TABLE keyed (id integer PRIMARY KEY, body text, tag integer, bytes bytea);
         CREATE INDEX keyed_tag ON keyed (tag)",
    );
    run(
        &engine,
        "INSERT INTO keyed SELECT g, 'row ' || g, g % 3, ('b' || g)::bytea FROM generate_series(1, 9) AS g",
    );
    engine
}

#[test]
fn supplied_identities_are_written_whether_or_not_their_table_ever_used_them() {
    for layout in LAYOUTS {
        let directory = tempfile::tempdir().unwrap();
        let engine = keyed_table(layout, &directory.path().join("supplied-identities.db"));
        assert_eq!(tagged(&engine, 0), ["3", "6", "9"], "{layout:?}");
        // A freed identity was used: its insert meets the records the deleted row left.
        run(&engine, "DELETE FROM keyed WHERE id IN (5, 6, 7)");
        run(
            &engine,
            "INSERT INTO keyed VALUES (6, 'again', 2, 'six'::bytea)",
        );
        // One statement supplies a freed identity and identities above the watermark, in no order.
        run(
            &engine,
            "INSERT INTO keyed VALUES (30, 'thirty', 0, NULL), (5, 'five', 1, 'five'::bytea), (25, 'twenty-five', 0, NULL)",
        );
        assert_eq!(
            keyed(&engine)[4..],
            [
                "5|five|1",
                "6|again|2",
                "8|row 8|2",
                "9|row 9|0",
                "25|twenty-five|0",
                "30|thirty|0"
            ],
            "{layout:?}"
        );
        assert_eq!(tagged(&engine, 0), ["3", "9", "25", "30"], "{layout:?}");
        assert_eq!(tagged(&engine, 2), ["2", "6", "8"], "{layout:?}");
        // An identity used and freed in one transaction is met in the transaction's own changes.
        run(
            &engine,
            "BEGIN;
             INSERT INTO keyed VALUES (40, 'first', 1, 'a'::bytea);
             DELETE FROM keyed WHERE id = 40;
             INSERT INTO keyed VALUES (40, 'second', 2, NULL);
             SAVEPOINT branch;
             INSERT INTO keyed VALUES (41, 'undone', 0, 'b'::bytea);
             ROLLBACK TO SAVEPOINT branch;
             INSERT INTO keyed VALUES (41, 'kept', 1, NULL);
             COMMIT",
        );
        // A rolled back insert raised the watermark and left no row, so its identity is no longer above it.
        run(
            &engine,
            "BEGIN; INSERT INTO keyed VALUES (50, 'discarded', 1, 'c'::bytea); ROLLBACK",
        );
        run(&engine, "INSERT INTO keyed VALUES (50, 'kept', 1, NULL)");
        assert_eq!(
            keyed(&engine)[10..],
            ["40|second|2", "41|kept|1", "50|kept|1"],
            "{layout:?}"
        );
        assert_eq!(
            rows(
                &engine,
                "SELECT id, bytes FROM keyed WHERE id IN (5, 40, 41, 50) ORDER BY id",
                &["id", "bytes"]
            ),
            ["5|[102, 105, 118, 101]", "40|null", "41|null", "50|null"],
            "{layout:?}"
        );
    }
}

#[test]
fn conflicts_and_key_changes_meet_the_rows_their_identities_name() {
    for layout in LAYOUTS {
        let directory = tempfile::tempdir().unwrap();
        let engine = keyed_table(layout, &directory.path().join("conflicting-identities.db"));
        // A key the table holds is still a conflict, in one statement and across statements, also above the watermark.
        for duplicate in [
            "INSERT INTO keyed VALUES (60, 'x', 1, NULL), (60, 'y', 1, NULL)",
            "INSERT INTO keyed VALUES (61, 'x', 1, NULL), (5, 'y', 1, NULL)",
        ] {
            assert_eq!(
                state(&engine, duplicate).as_deref(),
                Some("23505"),
                "{layout:?}"
            );
        }
        assert_eq!(keyed(&engine).len(), 9, "{layout:?}");
        // A statement that resolves conflicts rewrites the row it meets and inserts the others.
        run(
            &engine,
            "INSERT INTO keyed VALUES (6, 'resolved', 0, NULL), (70, 'new', 2, NULL)
             ON CONFLICT (id) DO UPDATE SET body = EXCLUDED.body, tag = EXCLUDED.tag",
        );
        run(
            &engine,
            "INSERT INTO keyed VALUES (6, 'ignored', 1, NULL), (71, 'kept', 1, NULL) ON CONFLICT DO NOTHING",
        );
        // A changed key moves its row to an identity that may have been used.
        run(&engine, "DELETE FROM keyed WHERE id = 7");
        run(&engine, "UPDATE keyed SET id = 7 WHERE id = 70");
        run(&engine, "UPDATE keyed SET id = 80 WHERE id = 71");
        assert_eq!(
            keyed(&engine)[5..],
            [
                "6|resolved|0",
                "7|new|2",
                "8|row 8|2",
                "9|row 9|0",
                "80|kept|1"
            ],
            "{layout:?}"
        );
        assert_eq!(tagged(&engine, 1), ["1", "4", "80"], "{layout:?}");
        assert_eq!(tagged(&engine, 2), ["2", "5", "7", "8"], "{layout:?}");
    }
}

#[test]
fn a_truncated_table_starts_a_generation_that_no_row_ever_used() {
    for layout in LAYOUTS {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("truncated-identities.db");
        let engine = keyed_table(layout, &path);
        run(&engine, "DELETE FROM keyed WHERE id > 6");
        // The new storage generation holds no record, whatever identities the last one used and freed.
        run(&engine, "TRUNCATE keyed");
        run(
            &engine,
            "INSERT INTO keyed SELECT g, 'new ' || g, g, NULL FROM generate_series(1, 4) AS g",
        );
        // A rolled back truncation returns the table to the generation its rows are in.
        run(
            &engine,
            "BEGIN; TRUNCATE keyed; INSERT INTO keyed VALUES (2, 'private', 2, NULL), (8, 'private', 8, NULL); ROLLBACK",
        );
        run(&engine, "DELETE FROM keyed WHERE id = 2");
        run(
            &engine,
            "INSERT INTO keyed VALUES (2, 'reused', 9, NULL), (8, 'eight', 9, NULL)",
        );
        let expected = [
            "1|new 1|1",
            "2|reused|9",
            "3|new 3|3",
            "4|new 4|4",
            "8|eight|9",
        ];
        assert_eq!(keyed(&engine), expected, "{layout:?}");
        assert_eq!(tagged(&engine, 9), ["2", "8"], "{layout:?}");
        drop(engine);
        let reopened = open(layout, &path);
        assert_eq!(keyed(&reopened), expected, "{layout:?}");
        assert_eq!(tagged(&reopened, 9), ["2", "8"], "{layout:?}");
        run(
            &reopened,
            "INSERT INTO keyed VALUES (6, 'after reopen', 9, NULL)",
        );
        assert_eq!(tagged(&reopened, 9), ["2", "6", "8"], "{layout:?}");
    }
}

#[test]
fn generated_identities_are_written_for_plain_and_partitioned_tables() {
    for layout in LAYOUTS {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("generated-identities.db");
        let engine = open(layout, &path);
        run(
            &engine,
            "CREATE TABLE named (name text NOT NULL, tag integer);
             CREATE INDEX named_tag ON named (tag);
             CREATE TABLE measured (region text NOT NULL, amount integer) PARTITION BY LIST (region);
             CREATE TABLE measured_east PARTITION OF measured FOR VALUES IN ('east');
             CREATE TABLE measured_west PARTITION OF measured FOR VALUES IN ('west')",
        );
        let named = |engine: &Engine| {
            rows(
                engine,
                "SELECT name, tag FROM named ORDER BY name",
                &["name", "tag"],
            )
        };
        // A table that reserves its identities writes each row at one it never used.
        run(
            &engine,
            "INSERT INTO named SELECT 'n' || g, g % 2 FROM generate_series(1, 6) AS g",
        );
        run(&engine, "DELETE FROM named WHERE tag = 0");
        run(
            &engine,
            "BEGIN;
             INSERT INTO named VALUES ('p1', 0), ('p2', 1);
             SAVEPOINT branch;
             INSERT INTO named VALUES ('p3', 0);
             ROLLBACK TO SAVEPOINT branch;
             INSERT INTO named VALUES ('p4', 0);
             COMMIT;
             BEGIN; INSERT INTO named VALUES ('discarded', 0); ROLLBACK;
             INSERT INTO named VALUES ('last', 0)",
        );
        assert_eq!(
            named(&engine),
            ["last|0", "n1|1", "n3|1", "n5|1", "p1|0", "p2|1", "p4|0"],
            "{layout:?}"
        );
        assert_eq!(
            rows(
                &engine,
                "SELECT name FROM named WHERE tag = 0 ORDER BY name",
                &["name"]
            ),
            ["last", "p1", "p4"],
            "{layout:?}"
        );
        // A partition draws its identities from its hierarchy, and a row that changes its partition is written at one of them.
        run(
            &engine,
            "INSERT INTO measured VALUES ('east', 1), ('west', 2), ('east', 3)",
        );
        run(
            &engine,
            "UPDATE measured SET region = 'west' WHERE amount = 1",
        );
        run(&engine, "DELETE FROM measured WHERE amount = 2");
        run(
            &engine,
            "INSERT INTO measured VALUES ('west', 4), ('east', 5)",
        );
        let measured = |engine: &Engine| {
            rows(
                engine,
                "SELECT region, amount FROM measured ORDER BY amount",
                &["region", "amount"],
            )
        };
        let expected = ["west|1", "east|3", "west|4", "east|5"];
        assert_eq!(measured(&engine), expected, "{layout:?}");
        drop(engine);
        let reopened = open(layout, &path);
        assert_eq!(measured(&reopened), expected, "{layout:?}");
        run(&reopened, "INSERT INTO named VALUES ('after reopen', 0)");
        assert_eq!(named(&reopened).len(), 8, "{layout:?}");
    }
}

#[test]
fn serial_and_identity_keys_are_written_as_the_unique_identities_they_are() {
    for layout in LAYOUTS {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sequence-identities.db");
        let engine = open(layout, &path);
        run(
            &engine,
            "CREATE TABLE serials (id serial PRIMARY KEY, body text, tag integer);
             CREATE INDEX serials_tag ON serials (tag);
             CREATE TABLE identities (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, body text)",
        );
        let serials = |engine: &Engine, tag: i64| {
            rows(
                engine,
                &format!("SELECT id, body FROM serials WHERE tag = {tag} ORDER BY id"),
                &["id", "body"],
            )
        };
        // Values drawn from the sequence lie above the table's watermark.
        run(
            &engine,
            "INSERT INTO serials (body, tag) SELECT 'row ' || g, g % 2 FROM generate_series(1, 6) AS g",
        );
        // A supplied key is validated as any other: one the table holds conflicts, a freed one is reused and one above the watermark is new.
        assert_eq!(
            state(
                &engine,
                "INSERT INTO serials (id, body, tag) VALUES (3, 'duplicate', 0)"
            )
            .as_deref(),
            Some("23505"),
            "{layout:?}"
        );
        run(&engine, "DELETE FROM serials WHERE id = 3");
        run(
            &engine,
            "INSERT INTO serials (id, body, tag) VALUES (3, 'again', 1), (20, 'far', 0)",
        );
        run(
            &engine,
            "INSERT INTO serials (body, tag) VALUES ('seven', 1)",
        );
        // The sequence does not follow supplied keys, so its next value meets the row that took it.
        run(
            &engine,
            "INSERT INTO serials (id, body, tag) VALUES (8, 'supplied', 0)",
        );
        assert_eq!(
            state(
                &engine,
                "INSERT INTO serials (body, tag) VALUES ('collides', 0)"
            )
            .as_deref(),
            Some("23505"),
            "{layout:?}"
        );
        run(
            &engine,
            "INSERT INTO serials (body, tag) VALUES ('nine', 1)",
        );
        let odd = ["1|row 1", "3|again", "5|row 5", "7|seven", "9|nine"];
        let even = ["2|row 2", "4|row 4", "6|row 6", "8|supplied", "20|far"];
        assert_eq!(serials(&engine, 1), odd, "{layout:?}");
        assert_eq!(serials(&engine, 0), even, "{layout:?}");
        run(
            &engine,
            "INSERT INTO identities (body) VALUES ('one'), ('two')",
        );
        assert_eq!(
            state(
                &engine,
                "INSERT INTO identities (id, body) VALUES (2, 'duplicate')"
            )
            .as_deref(),
            Some("23505"),
            "{layout:?}"
        );
        run(&engine, "DELETE FROM identities WHERE id = 1");
        run(
            &engine,
            "INSERT INTO identities (id, body) VALUES (1, 'again'), (5, 'five')",
        );
        let identities = ["1|again", "2|two", "5|five"];
        let listed = |engine: &Engine| {
            rows(
                engine,
                "SELECT id, body FROM identities ORDER BY id",
                &["id", "body"],
            )
        };
        assert_eq!(listed(&engine), identities, "{layout:?}");
        drop(engine);
        let reopened = open(layout, &path);
        assert_eq!(serials(&reopened, 1), odd, "{layout:?}");
        assert_eq!(serials(&reopened, 0), even, "{layout:?}");
        assert_eq!(listed(&reopened), identities, "{layout:?}");
    }
}

#[test]
fn an_identity_key_its_sequence_draws_meets_the_records_of_the_row_that_used_it() {
    for layout in LAYOUTS {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reissued-identities.db");
        let engine = open(layout, &path);
        run(
            &engine,
            "CREATE TABLE reissued (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, v integer, w text);
             CREATE INDEX reissued_w ON reissued (w)",
        );
        // The sequence draws identity 1 after a deleted row used it, so the table did not reserve it.
        run(&engine, "INSERT INTO reissued VALUES (1, 1, 'one')");
        run(&engine, "DELETE FROM reissued WHERE id = 1");
        run(&engine, "INSERT INTO reissued (v, w) VALUES (2, 'two')");
        // It draws identity 2 after a row of the same transaction used and freed it.
        run(
            &engine,
            "BEGIN;
             INSERT INTO reissued VALUES (2, 3, 'three');
             DELETE FROM reissued WHERE id = 2;
             INSERT INTO reissued (v, w) VALUES (4, 'four');
             COMMIT",
        );
        let check = |engine: &Engine| {
            assert_eq!(
                rows(
                    engine,
                    "SELECT id, v, w FROM reissued ORDER BY id",
                    &["id", "v", "w"]
                ),
                ["1|2|two", "2|4|four"],
                "{layout:?}"
            );
            assert!(
                rows(
                    engine,
                    "SELECT id FROM reissued WHERE w IN ('one', 'three')",
                    &["id"]
                )
                .is_empty(),
                "{layout:?}"
            );
            assert_eq!(
                rows(engine, "SELECT id FROM reissued WHERE w = 'four'", &["id"]),
                ["2"],
                "{layout:?}"
            );
        };
        check(&engine);
        drop(engine);
        check(&open(layout, &path));
    }
}

#[test]
fn an_identity_another_session_used_and_freed_while_the_statement_ran_is_written_over_what_it_left()
{
    for layout in LAYOUTS {
        let directory = tempfile::tempdir().unwrap();
        let engine = keyed_table(layout, &directory.path().join("interleaved-identities.db"));
        let other = std::sync::Mutex::new(engine.new_session().unwrap());
        let calls = std::sync::atomic::AtomicUsize::new(0);
        // While the statement evaluates its first row, another session inserts and deletes the identity of its second.
        engine
            .register_scalar_function_with_options(
                "interleave",
                uqa_engine::SQLFunctionOptions::read_only(
                    uqa_engine::SQLFunctionVolatility::Volatile,
                ),
                move |arguments: &[Value]| {
                    if calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0 {
                        other
                            .lock()
                            .unwrap()
                            .sql(
                                "INSERT INTO keyed VALUES (21, 'theirs', 1, 'x'::bytea);
                                 DELETE FROM keyed WHERE id = 21",
                                &[],
                            )
                            .map_err(|error| uqa_sql::SQLError::Internal(error.to_string()))?;
                    }
                    Ok(arguments[0].clone())
                },
            )
            .unwrap();
        // The statement began before that commit. It writes identity 21 over the revision the deleted row left there, and 22 at an identity nobody used.
        run(
            &engine,
            "INSERT INTO keyed SELECT interleave(g), 'mine ' || g, 2, NULL FROM generate_series(20, 22) AS g",
        );
        assert_eq!(
            keyed(&engine)[9..],
            ["20|mine 20|2", "21|mine 21|2", "22|mine 22|2"],
            "{layout:?}"
        );
        assert_eq!(
            tagged(&engine, 2),
            ["2", "5", "8", "20", "21", "22"],
            "{layout:?}"
        );
        assert_eq!(tagged(&engine, 1), ["1", "4", "7"], "{layout:?}");
    }
}
