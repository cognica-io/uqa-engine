//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for date and time input: the special values `now`, `today`, `tomorrow`, `yesterday`, `epoch` and `allballs` resolve against the transaction start so `'now'::timestamp = now()::timestamp` holds, `DEFAULT 'now'` is read when the table is created and printed as the creation time, stored temporal constants print through the output functions (`'01:00:00'::interval`), a generation expression reads `ts + '1 day'` with the operator the catalog selects, the input functions report `22007` `invalid input syntax`, `22008` `date/time field value out of range` with the `DateStyle` hint for a month or day out of order, `22009` `time zone displacement out of range`, `22023` for an unknown zone name and `22015` `interval field value out of range`, `time + date` and `timetz + date` produce timestamps, years before the common era read and print with `BC` and years past 9999 print without a sign, a written cast of a literal in a stored expression prints as the constant the input function read with `format_type`'s spelling (`'11:00:00'::time(3) without time zone`, `'\x79'::bytea`), the `~`, `~*`, `!~` and `!~*` operators name their output `?column?` and print as operators while `regexp_like` stays a call, and a value of another type has no cast to a temporal type (`42846`).

use tempfile::TempDir;
use uqa_engine::Engine;

fn verify_datetime_input(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/datetime_input_oracle.expected.json"),
    );
}

#[test]
fn datetime_input_matches_postgresql_memory() {
    verify_datetime_input(&Engine::new());
}

#[test]
fn datetime_input_matches_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_datetime_input(&Engine::open(&directory.path().join("datetime-input.db")).unwrap());
}
