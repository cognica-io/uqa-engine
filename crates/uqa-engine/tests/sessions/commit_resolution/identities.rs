//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A statement observes the identities it supplies with one physical allocation.

use super::*;

/// The identifier observations the foreground thread made while `work` ran.
fn observations(persistence: &FaultPersistence, work: impl FnOnce()) -> usize {
    let observed = || {
        persistence
            .foreground_identifier_observations
            .load(Ordering::Acquire)
    };
    let before = observed();
    work();
    observed() - before
}

#[test]
fn a_statement_observes_the_greatest_identity_it_supplies_once() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let engine = engine(persistence.clone());
        let run = |statement: &str| {
            engine.sql(statement, &[]).unwrap();
        };
        run("CREATE TABLE keyed (id integer PRIMARY KEY, value integer NOT NULL)");
        // Ascending identities would each raise the watermark; the statement raises it once, to the greatest.
        assert_eq!(
            observations(&persistence, || run(
                "INSERT INTO keyed SELECT g, g FROM generate_series(1, 50) AS g"
            )),
            1
        );
        assert_eq!(
            observations(&persistence, || run(
                "INSERT INTO keyed VALUES (60, 1), (70, 2), (65, 3)"
            )),
            1
        );
        // Identities at or below the watermark the session has read raise nothing.
        assert_eq!(
            observations(&persistence, || run(
                "INSERT INTO keyed VALUES (55, 1), (52, 2)"
            )),
            0
        );
        assert_eq!(
            observations(&persistence, || run("UPDATE keyed SET value = value + 1")),
            0
        );
        assert_eq!(count(&engine, "keyed"), Value::Int(55));
        // A table that generates its identities reserves them, which already covers each row.
        run("CREATE TABLE named (name text NOT NULL)");
        assert_eq!(
            observations(&persistence, || run(
                "INSERT INTO named SELECT 'n' || g FROM generate_series(1, 20) AS g"
            )),
            0
        );
        assert_eq!(count(&engine, "named"), Value::Int(20));
    }
}
