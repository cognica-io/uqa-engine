//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Robustness fuzz for `uqa_sql::compile`. Phase 11 Section 7.5 calls
//! out random-input fuzzing of the SQL parser; this is the stable-Rust
//! variant — no `cargo fuzz`, no nightly. We drive `compile` with a
//! mix of pure-random byte strings and randomly stitched fragments of
//! near-SQL syntax. The harness asserts every input either succeeds
//! cleanly or returns a `SQLError` — no panics, no UB, no timeouts.

use proptest::prelude::*;
use uqa_sql::compile;

/// Drive the parser with a payload and assert no panic. Either
/// `Ok(_)` (parsed) or `Err(SQLError::*)` is acceptable — only a
/// panic is a fuzz failure.
fn drive(sql: &str) -> Result<(), TestCaseError> {
    match std::panic::catch_unwind(|| compile(sql)) {
        Ok(_) => Ok(()),
        Err(_) => Err(TestCaseError::fail(format!(
            "compile panicked on input: {sql:?}"
        ))),
    }
}

/// A small, hand-curated lexicon of SQL fragments. The fuzz strategy
/// stitches random subsets of these into longer strings, which tends
/// to surface lexer/parser corner cases (unbalanced quotes,
/// runaway identifiers, near-keywords) much faster than purely
/// uniform random bytes.
const FRAGMENTS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "JOIN", "INNER", "LEFT", "RIGHT", "AND", "OR", "NOT", "NULL",
    "TRUE", "FALSE", "CREATE", "TABLE", "INDEX", "DROP", "INSERT", "UPDATE", "DELETE", "GROUP",
    "BY", "ORDER", "LIMIT", "OFFSET", "VALUES", "(", ")", ",", ";", "*", "=", "<", ">", "<=", ">=",
    "<>", "!=", "1", "2", "100", "0", "-1", "'", "\"", "\\", "--", "/*", "*/", "id", "name",
    "body", "score", "vec", " ", "\t", "\n",
    // Some near-keyword identifiers that keyword-followed parsers
    // sometimes mis-handle.
    "selct", "form", "wher", "froom",
];

fn fragment_strategy() -> impl Strategy<Value = String> {
    proptest::collection::vec(0usize..FRAGMENTS.len(), 1..40).prop_map(|idxs| {
        let mut s = String::new();
        for i in idxs {
            s.push_str(FRAGMENTS[i]);
        }
        s
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        ..ProptestConfig::default()
    })]

    /// Pure ASCII random strings — small enough that proptest can
    /// shrink to a minimal repro, large enough to exercise both the
    /// lexer and the parser.
    #[test]
    fn random_ascii_does_not_panic(s in "[\\x20-\\x7e]{0,128}") {
        drive(&s)?;
    }

    /// Random byte vectors converted via lossy UTF-8. This catches
    /// bugs that only fire on non-ASCII inputs (e.g. char-boundary
    /// slicing in the lexer).
    #[test]
    fn random_bytes_do_not_panic(bytes in proptest::collection::vec(any::<u8>(), 0..128)) {
        let s = String::from_utf8_lossy(&bytes);
        drive(&s)?;
    }

    /// Stitched near-SQL fragments. Most of these will fail to parse,
    /// but they reach much deeper into the parser than truly random
    /// bytes do, since the lexer accepts most of them as tokens.
    #[test]
    fn stitched_fragments_do_not_panic(s in fragment_strategy()) {
        drive(&s)?;
    }
}

/// Concrete corpus of historically-tricky inputs. Anything we have
/// previously had a bug for goes here as a regression test.
#[test]
fn known_pathological_inputs() {
    let cases = [
        "",
        " ",
        ";",
        ";;;",
        "SELECT",
        "SELECT;",
        "SELECT *;",
        "SELECT * FROM",
        "SELECT * FROM t WHERE",
        "/* unterminated",
        "-- trailing",
        "''",
        "''''",
        "\"\"",
        "(((((((",
        ")))))))",
        "SELECT 1/0",
        "SELECT 1, 2, 3 FROM t WHERE x IN (1, 2, 3) GROUP BY id ORDER BY id LIMIT 5 OFFSET 0",
    ];
    for c in &cases {
        let _ = compile(c);
    }
}
