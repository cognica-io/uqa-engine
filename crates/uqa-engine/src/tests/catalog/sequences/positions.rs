//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A persistent sequence hands out the values `PostgreSQL` does while its durable record runs ahead of them.

use crate::tests::relation_lock_support::{reopen, sessions, sql};
use crate::Engine;
use uqa_core::{RelationIdentity, Value};
use uqa_execution::row_locks::{SequencePosition, SequencePositionKey};

const DATABASE: &str = "table-locks.db";

fn integer(engine: &Engine, statement: &str) -> i64 {
    match sql(engine, statement).rows[0].values().next() {
        Some(Value::Int(value)) => *value,
        other => panic!("{statement}: expected an integer, got {other:?}"),
    }
}

fn next(engine: &Engine, name: &str) -> i64 {
    integer(engine, &format!("SELECT nextval('{name}')"))
}

/// `last_value`, `log_cnt` and `is_called` as a query of the sequence shows them.
fn shown(engine: &Engine, name: &str) -> (i64, i64, bool) {
    let result = sql(
        engine,
        &format!("SELECT last_value, log_cnt, is_called FROM {name}"),
    );
    let row = &result.rows[0];
    match (&row["last_value"], &row["log_cnt"], &row["is_called"]) {
        (Value::Int(last), Value::Int(log), Value::Bool(called)) => (*last, *log, *called),
        other => panic!("unexpected sequence row {other:?}"),
    }
}

/// The value state of the durable record.
fn record(engine: &Engine, name: &str) -> (i64, bool, i64) {
    let relation = RelationIdentity::new("public", name);
    let row = engine
        .new_session()
        .unwrap()
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .load_sequence_rows()
        .unwrap()
        .into_iter()
        .find(|row| row.relation == relation)
        .unwrap();
    (row.current, row.called, row.log_count)
}

fn position_key(engine: &Engine, name: &str) -> SequencePositionKey {
    let relation = RelationIdentity::new("public", name);
    engine.refresh_sequences_from_catalog().unwrap();
    let state = engine.durable.sequences.read()[&relation];
    state.position_key(engine.durable.sequence_object_ids.read()[&relation])
}

/// The recorded position of a sequence with whether it was written in this run.
fn position(engine: &Engine, name: &str) -> Option<(SequencePosition, bool)> {
    engine
        .row_locks
        .sequence_position(position_key(engine, name))
        .unwrap()
        .map(|recorded| (recorded.position, recorded.fresh))
}

#[test]
fn sessions_draw_consecutive_values_while_the_record_is_written_once_for_those_it_covers() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        sql(&engine, "CREATE SEQUENCE ids");
        assert_eq!(shown(&peer, "ids"), (1, 0, false), "{provider}");
        assert_eq!(record(&engine, "ids"), (1, false, 0), "{provider}");
        for expected in 1..=70 {
            let session = if expected % 2 == 0 { &engine } else { &peer };
            assert_eq!(next(session, "ids"), expected, "{provider}");
            // `PostgreSQL` logs at the first call and again whenever the 32 values it logged ahead are used up.
            let logged_at = 1 + (expected - 1) / 33 * 33;
            for reader in [&engine, &peer] {
                assert_eq!(
                    shown(reader, "ids"),
                    (expected, 32 - (expected - logged_at), true),
                    "{provider}"
                );
            }
            assert_eq!(
                record(&engine, "ids"),
                (logged_at + 32, true, 0),
                "{provider}: {expected}"
            );
        }
        // Every reader of a sequence's value reads the exact position.
        assert_eq!(
            integer(
                &engine,
                "SELECT last_value FROM pg_sequences WHERE sequencename = 'ids'"
            ),
            70
        );
        assert_eq!(integer(&engine, "SELECT pg_sequence_last_value('ids')"), 70);
        let state = engine.sequence_state("ids").unwrap().unwrap().1;
        assert_eq!(
            (state.current, state.called, state.log_count),
            (70, true, 29)
        );
        let state = engine.sequences_snapshot().unwrap()["public.ids"];
        assert_eq!(
            (state.current, state.called, state.log_count),
            (70, true, 29)
        );
    }
}

#[test]
fn an_orderly_reopen_continues_every_sequence_exactly() {
    for provider in 0..3 {
        let (directory, engine, peer) = sessions(provider);
        sql(&engine, "CREATE SEQUENCE ids; CREATE SEQUENCE untouched");
        for expected in 1..=5 {
            assert_eq!(next(&peer, "ids"), expected);
        }
        drop(peer);
        drop(engine);
        let engine = reopen(provider, &directory.path().join(DATABASE));
        // The position was written in the earlier run and still continues the record.
        let (kept, fresh) = position(&engine, "ids").unwrap();
        assert_eq!((kept.current, kept.logged, fresh), (5, (33, true), false));
        assert_eq!(shown(&engine, "ids"), (5, 28, true), "{provider}");
        assert_eq!(next(&engine, "ids"), 6, "{provider}");
        assert_eq!(shown(&engine, "ids"), (6, 27, true), "{provider}");
        assert!(position(&engine, "ids").unwrap().1);
        assert_eq!(record(&engine, "ids"), (33, true, 0));
        assert_eq!(next(&engine, "untouched"), 1);
    }
}

#[test]
fn a_database_that_lost_its_positions_continues_past_its_records() {
    for provider in 0..3 {
        let (directory, engine, peer) = sessions(provider);
        sql(
            &engine,
            "CREATE SEQUENCE ids; CREATE SEQUENCE blocks CACHE 10",
        );
        for expected in 1..=5 {
            assert_eq!(next(&engine, "ids"), expected);
        }
        assert_eq!(next(&engine, "blocks"), 1);
        assert_eq!(record(&engine, "blocks"), (42, true, 0));
        drop(peer);
        drop(engine);
        // A machine failure leaves positions that may be older than the values handed out, so none is kept.
        let mut positions = directory.path().join(DATABASE).into_os_string();
        positions.push(".uqa-sequences");
        std::fs::remove_file(&positions).unwrap();
        let engine = reopen(provider, &directory.path().join(DATABASE));
        assert_eq!(position(&engine, "ids"), None);
        // The record is what `PostgreSQL` replays from its log: the logged value, called, with nothing logged ahead.
        assert_eq!(shown(&engine, "ids"), (33, 0, true), "{provider}");
        assert_eq!(next(&engine, "ids"), 34, "{provider}");
        assert_eq!(shown(&engine, "ids"), (34, 32, true), "{provider}");
        assert_eq!(record(&engine, "ids"), (66, true, 0));
        assert_eq!(next(&engine, "blocks"), 43, "{provider}");
    }
}

fn copy_database(from: &std::path::Path, to: &std::path::Path) {
    for entry in std::fs::read_dir(to).unwrap() {
        let path = entry.unwrap().path();
        if !path.to_string_lossy().contains(".uqa-") {
            std::fs::remove_file(path).unwrap();
        }
    }
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        if !path.to_string_lossy().contains(".uqa-") {
            std::fs::copy(&path, to.join(path.file_name().unwrap())).unwrap();
        }
    }
}

#[test]
fn positions_kept_for_a_database_file_that_was_replaced_are_not_continued() {
    for provider in 0..3 {
        let (directory, engine, peer) = sessions(provider);
        let path = directory.path().join(DATABASE);
        sql(&engine, "CREATE SEQUENCE ids");
        for expected in 1..=3 {
            assert_eq!(next(&engine, "ids"), expected);
        }
        drop(peer);
        drop(engine);
        let earlier = tempfile::tempdir().unwrap();
        copy_database(directory.path(), earlier.path());
        let engine = reopen(provider, &path);
        for expected in 4..=40 {
            assert_eq!(next(&engine, "ids"), expected);
        }
        assert_eq!(record(&engine, "ids"), (66, true, 0));
        drop(engine);
        // The earlier file returns while the sidecar still holds the later position.
        copy_database(earlier.path(), directory.path());
        let engine = reopen(provider, &path);
        assert_eq!(record(&engine, "ids"), (33, true, 0), "{provider}");
        let (kept, fresh) = position(&engine, "ids").unwrap();
        assert_eq!((kept.current, kept.logged, fresh), (40, (66, true), false));
        assert_eq!(shown(&engine, "ids"), (33, 0, true), "{provider}");
        assert_eq!(next(&engine, "ids"), 34, "{provider}");
        let (kept, fresh) = position(&engine, "ids").unwrap();
        assert_eq!((kept.current, kept.logged, fresh), (34, (66, true), true));
    }
}

#[test]
fn an_assigned_value_replaces_the_position() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        sql(&engine, "CREATE SEQUENCE ids");
        for expected in 1..=3 {
            assert_eq!(next(&engine, "ids"), expected);
        }
        assert_eq!(integer(&peer, "SELECT setval('ids', 100)"), 100);
        assert_eq!(position(&engine, "ids"), None);
        assert_eq!(record(&engine, "ids"), (100, true, 0));
        assert_eq!(shown(&engine, "ids"), (100, 0, true), "{provider}");
        assert_eq!(next(&engine, "ids"), 101, "{provider}");
        assert_eq!(shown(&peer, "ids"), (101, 32, true), "{provider}");
        assert_eq!(integer(&engine, "SELECT setval('ids', 7, false)"), 7);
        assert_eq!(shown(&peer, "ids"), (7, 0, false), "{provider}");
        assert_eq!(next(&peer, "ids"), 7, "{provider}");
        assert_eq!(next(&engine, "ids"), 8, "{provider}");
        // An assignment inside a transaction that rolls back stays assigned, as in `PostgreSQL`.
        sql(&engine, "BEGIN; SELECT setval('ids', 500); ROLLBACK");
        assert_eq!(next(&peer, "ids"), 501, "{provider}");
    }
}

#[test]
fn a_replaced_definition_continues_the_exact_position() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        sql(&engine, "CREATE SEQUENCE ids");
        for expected in 1..=3 {
            assert_eq!(next(&peer, "ids"), expected);
        }
        // The durable record holds 33, and the new definition continues from 3.
        sql(&engine, "ALTER SEQUENCE ids INCREMENT 10");
        assert_eq!(shown(&peer, "ids"), (3, 0, true), "{provider}");
        assert_eq!(next(&peer, "ids"), 13, "{provider}");
        assert_eq!(next(&engine, "ids"), 23, "{provider}");
        // A definition change that rolls back leaves the position of the definition it did not replace.
        sql(
            &engine,
            "BEGIN; ALTER SEQUENCE ids INCREMENT 1000; SELECT nextval('ids'); ROLLBACK",
        );
        assert_eq!(next(&peer, "ids"), 33, "{provider}");
        assert_eq!(shown(&engine, "ids"), (33, 30, true), "{provider}");
        sql(&engine, "ALTER SEQUENCE ids RESTART");
        assert_eq!(shown(&peer, "ids"), (1, 0, false), "{provider}");
        assert_eq!(next(&peer, "ids"), 1, "{provider}");
        sql(&engine, "ALTER SEQUENCE ids OWNED BY t.v");
        assert_eq!(next(&peer, "ids"), 11, "{provider}");
    }
}

#[test]
fn a_sequence_changed_by_a_transaction_continues_exactly_in_it_and_after_it() {
    for provider in 0..3 {
        for finish in ["COMMIT", "ROLLBACK"] {
            let (_directory, engine, peer) = sessions(provider);
            sql(
                &engine,
                "CREATE ROLE reader; CREATE SEQUENCE ids; CREATE SEQUENCE moved",
            );
            for expected in 1..=3 {
                assert_eq!(next(&peer, "ids"), expected);
                assert_eq!(next(&peer, "moved"), expected);
            }
            sql(
                &engine,
                "BEGIN; GRANT USAGE ON SEQUENCE ids TO reader; ALTER SEQUENCE moved RENAME TO renamed",
            );
            assert_eq!(shown(&engine, "ids"), (3, 30, true), "{provider}");
            assert_eq!(next(&engine, "ids"), 4, "{provider}: {finish}");
            assert_eq!(next(&engine, "renamed"), 4, "{provider}: {finish}");
            assert_eq!(next(&engine, "renamed"), 5, "{provider}: {finish}");
            assert_eq!(shown(&engine, "renamed"), (5, 28, true), "{provider}");
            sql(&engine, finish);
            // Values drawn in a transaction are never drawn again, whether it commits or not.
            let name = if finish == "COMMIT" {
                "renamed"
            } else {
                "moved"
            };
            assert_eq!(next(&peer, name), 6, "{provider}: {finish}");
            assert_eq!(next(&peer, "ids"), 5, "{provider}: {finish}");
            assert_eq!(next(&engine, "ids"), 6, "{provider}: {finish}");
        }
    }
}

#[test]
fn a_privilege_or_owner_change_keeps_the_durable_record() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        sql(&engine, "CREATE ROLE reader; CREATE SEQUENCE ids");
        for expected in 1..=3 {
            assert_eq!(next(&engine, "ids"), expected);
        }
        // The session that drew the values holds their exact state, and none of it may reach the record.
        for change in [
            "GRANT USAGE ON SEQUENCE ids TO reader",
            "ALTER SEQUENCE ids OWNER TO reader",
            "ALTER SEQUENCE ids OWNED BY t.v",
            "ALTER SEQUENCE ids RENAME TO renamed; ALTER SEQUENCE renamed RENAME TO ids",
        ] {
            sql(&engine, change);
            assert_eq!(
                record(&engine, "ids"),
                (33, true, 0),
                "{provider}: {change}"
            );
            assert_eq!(shown(&peer, "ids"), (3, 30, true), "{provider}: {change}");
        }
        assert_eq!(next(&peer, "ids"), 4, "{provider}");
    }
}

#[test]
fn cached_blocks_and_bounds_follow_the_shared_position() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        sql(
            &engine,
            "CREATE SEQUENCE blocks CACHE 10; CREATE SEQUENCE laps MINVALUE 1 MAXVALUE 3 CYCLE; CREATE SEQUENCE few MAXVALUE 2; CREATE SEQUENCE down INCREMENT -2 MINVALUE -5 MAXVALUE 0",
        );
        assert_eq!(next(&engine, "blocks"), 1);
        assert_eq!(next(&peer, "blocks"), 11);
        assert_eq!(next(&engine, "blocks"), 2);
        assert_eq!(shown(&peer, "blocks"), (20, 22, true), "{provider}");
        for expected in [1, 2, 3, 1, 2, 3, 1] {
            assert_eq!(next(&peer, "laps"), expected, "{provider}");
        }
        assert_eq!(next(&engine, "few"), 1);
        assert_eq!(next(&peer, "few"), 2);
        let error = peer.sql("SELECT nextval('few')", &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("2200H"), "{provider}: {error}");
        for expected in [0, -2, -4] {
            assert_eq!(next(&engine, "down"), expected, "{provider}");
        }
        let error = engine.sql("SELECT nextval('down')", &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("2200H"), "{provider}: {error}");
    }
}

#[test]
fn a_recreated_sequence_starts_over_and_the_positions_of_missing_sequences_are_dropped() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        sql(&engine, "CREATE SEQUENCE ids; CREATE SEQUENCE kept");
        assert_eq!(next(&engine, "ids"), 1);
        assert_eq!(next(&engine, "kept"), 1);
        let dropped = position_key(&engine, "ids");
        sql(&engine, "DROP SEQUENCE ids; CREATE SEQUENCE ids");
        assert_eq!(next(&peer, "ids"), 1, "{provider}");
        // The dropped sequence's position stays until a sequence that needs a slot finds the store crowded.
        assert!(engine
            .row_locks
            .sequence_position(dropped)
            .unwrap()
            .is_some());
        for sequence in 0..1024_u32 {
            let mut object = [0xee_u8; 16];
            object[..4].copy_from_slice(&sequence.to_be_bytes());
            engine
                .row_locks
                .lock_sequence_position(SequencePositionKey {
                    object,
                    definition: [1; 16],
                })
                .unwrap()
                .record(SequencePosition {
                    logged: (33, true),
                    current: 1,
                    called: true,
                    log_count: 32,
                })
                .unwrap();
        }
        sql(&engine, "CREATE SEQUENCE late");
        assert_eq!(next(&peer, "late"), 1, "{provider}");
        let positions = engine.row_locks.sequence_positions().unwrap();
        assert_eq!(positions.len(), 3, "{provider}");
        assert!(!positions.contains_key(&dropped));
        assert_eq!(next(&engine, "kept"), 2, "{provider}");
        assert_eq!(next(&engine, "ids"), 2, "{provider}");
    }
}

#[test]
fn concurrent_sessions_never_draw_a_value_twice() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        sql(
            &engine,
            "CREATE SEQUENCE ids; CREATE SEQUENCE blocks CACHE 7",
        );
        let workers = (0..4)
            .map(|_| {
                let session = engine.new_session().unwrap();
                std::thread::spawn(move || {
                    (0..150)
                        .map(|_| (next(&session, "ids"), next(&session, "blocks")))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        let mut ids = Vec::new();
        let mut blocks = Vec::new();
        for worker in workers {
            for (id, block) in worker.join().unwrap() {
                ids.push(id);
                blocks.push(block);
            }
        }
        ids.sort_unstable();
        assert_eq!(ids, (1..=600).collect::<Vec<_>>(), "{provider}");
        blocks.sort_unstable();
        blocks.dedup();
        assert_eq!(blocks.len(), 600, "{provider}");
    }
}
