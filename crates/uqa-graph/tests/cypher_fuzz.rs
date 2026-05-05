//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Robustness fuzz for `uqa_graph::cypher::parse_cypher`. Phase 11
//! Section 7.5 calls out random-input fuzzing of the Cypher parser;
//! this is the stable-Rust variant — no `cargo fuzz`, no nightly. The
//! harness asserts every input either parses cleanly or returns a
//! `ParseError` — no panics, no UB, no timeouts.

use proptest::prelude::*;
use uqa_graph::cypher::parse_cypher;

fn drive(query: &str) -> Result<(), TestCaseError> {
    match std::panic::catch_unwind(|| parse_cypher(query)) {
        Ok(_) => Ok(()),
        Err(_) => Err(TestCaseError::fail(format!(
            "parse_cypher panicked on input: {query:?}"
        ))),
    }
}

const FRAGMENTS: &[&str] = &[
    "MATCH", "CREATE", "MERGE", "DELETE", "DETACH", "RETURN", "WHERE", "WITH", "ORDER", "BY",
    "LIMIT", "SKIP", "ASC", "DESC", "AS", "AND", "OR", "NOT", "NULL", "TRUE", "FALSE", "(", ")",
    "[", "]", "{", "}", "-", "->", "<-", "--", ":", ",", ".", "*", "=", "<>", "<", ">", "<=", ">=",
    "n", "m", "r", "v", "Person", "Knows", "Likes", "name", "age", "id", "'foo'", "\"bar\"", "1",
    "2", "100", "0.5", " ", "\t", "\n",
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

    #[test]
    fn random_ascii_does_not_panic(s in "[\\x20-\\x7e]{0,128}") {
        drive(&s)?;
    }

    #[test]
    fn random_bytes_do_not_panic(bytes in proptest::collection::vec(any::<u8>(), 0..128)) {
        let s = String::from_utf8_lossy(&bytes);
        drive(&s)?;
    }

    #[test]
    fn stitched_fragments_do_not_panic(s in fragment_strategy()) {
        drive(&s)?;
    }
}

/// Concrete pathological inputs.
#[test]
fn known_pathological_inputs() {
    let cases = [
        "",
        " ",
        "MATCH",
        "MATCH ()",
        "MATCH ()-",
        "MATCH ()-[",
        "MATCH ()-[]",
        "MATCH ()-[]-",
        "MATCH ()-[]-()",
        "MATCH ()-[]->()",
        "MATCH (n) RETURN",
        "MATCH (n) RETURN n LIMIT",
        "/* unterminated",
        "((((((",
        "[[[[[[",
        "{{{{{{",
        "MATCH (n {a: 1, b: 2}) RETURN n",
    ];
    for c in &cases {
        let _ = parse_cypher(c);
    }
}
