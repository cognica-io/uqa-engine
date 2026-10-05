//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for operator selection and the reading of `unknown` operands: an operator reads an `unknown` literal with the operand type it selects (`1 + '1'` stores `(a + 1)`, `1.5 + 'x'` and `true = 'x'` report the input function's `22P02`), `oper_select_candidate`'s last heuristic assumes the known operand's type after a category conflict (`time + '1 hour'` selects `time + interval`, `rk(1, 'x')` selects `rk(numeric, numeric)`), `-'1'`, `'1' + '2'` and `date + 'x'` report `42725` `operator is not unique` with its hint, `1 || 2` and `bytea || integer` report `42883` since `||` needs a text operand, an array pair, a `bytea` pair or a `jsonb` pair, the `unknown` operand of `||` takes the typed operand's type (`bytea || 'y'` joins bytes, `jsonb || '{"a":1}'` stores the `jsonb` constant, `int[] || '{2}'` the array), defaults, generated columns, views, CHECK constraints and SQL function bodies store and print the read constants, a domain's default is stored in `typdefaultbin`, and a value past a numeric column's precision reports `numeric field overflow` with the DETAIL naming the magnitude the field admits.

use tempfile::TempDir;
use uqa_engine::Engine;

fn verify_operator_diagnostics(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/operator_diagnostics_oracle.expected.json"),
    );
}

#[test]
fn operator_diagnostics_match_postgresql_memory() {
    verify_operator_diagnostics(&Engine::new());
}

#[test]
fn operator_diagnostics_match_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_operator_diagnostics(
        &Engine::open(&directory.path().join("operator-diagnostics.db")).unwrap(),
    );
}
