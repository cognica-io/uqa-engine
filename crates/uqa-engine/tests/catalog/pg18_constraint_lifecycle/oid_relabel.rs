//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for comparisons on `oid` and its alias types: `regclass`, `regtype`, `regproc` and `regnamespace` operands take the `oid` operators, so a CHECK constraint, a view and a plain statement store and print the relabels `(a)::oid <> ('t'::regclass)::oid`, an integer operand prints its cast `(16384)::oid`, an `unknown` literal is read by `oidin` (`'16384'::oid`, `22P02` for a name), `BETWEEN`, `IS DISTINCT FROM`, `NULLIF`, `CASE` tests and `= ANY` over a `regclass[]` value relabel the same way, `max` and `min` of an alias column return `oid`, `oidin` reads text as `strtoul` with base 0, and rows of alias columns hold the objects' OIDs, so comparisons and `IN` lists resolve names before comparing.

use tempfile::TempDir;
use uqa_engine::Engine;

fn verify_oid_relabel(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../../tests/parity/pg18/oid_relabel_oracle.expected.json"),
    );
}

#[test]
fn oid_relabel_matches_postgresql_memory() {
    verify_oid_relabel(&Engine::new());
}

#[test]
fn oid_relabel_matches_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_oid_relabel(&Engine::open(&directory.path().join("oid-relabel.db")).unwrap());
}
