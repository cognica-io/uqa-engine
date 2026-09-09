//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Robustness fuzz for `uqa_graph::cypher::parse_cypher`. The master
//! plan calls out random-input fuzzing of the Cypher parser; this is
//! the stable-Rust variant - no `cargo fuzz`, no nightly. The
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

#[test]
fn deeply_nested_expressions_return_parse_errors() {
    for (prefix, suffix) in [
        ("[", "]"),
        ("(", ")"),
        ("{value: ", "}"),
        ("coalesce(", ")"),
        ("CASE WHEN true THEN ", " ELSE 0 END"),
        ("NOT ", ""),
        ("- ", ""),
    ] {
        let query = format!("RETURN {}0{}", prefix.repeat(4_096), suffix.repeat(4_096));
        let error = parse_cypher(&query).expect_err("excessive expression nesting must fail");
        assert!(
            error.to_string().contains("expression nesting limit"),
            "{error}"
        );
    }
    let malformed = format!("SET {}NULL", "[".repeat(4_096));
    assert!(parse_cypher(&malformed).is_err());
}

#[test]
fn long_expression_chains_return_parse_errors() {
    for suffix in [
        " + 0",
        " * 0",
        " ^ 0",
        " AND true",
        " OR true",
        " XOR true",
        " IS NULL",
        " IN []",
        " < 0",
        "[0]",
        "[..]",
        ".value",
    ] {
        let query = format!("RETURN 0{}", suffix.repeat(4_096));
        let error = parse_cypher(&query).expect_err("excessive expression depth must fail");
        assert!(
            error.to_string().contains("expression nesting limit"),
            "{error}"
        );
    }
}

#[test]
fn wide_expressions_and_independent_projections_remain_valid() {
    let elements = vec!["0"; 10_000].join(", ");
    assert!(parse_cypher(&format!("RETURN [{elements}]")).is_ok());
    let nested = format!("{}0{}", "[".repeat(16), "]".repeat(16));
    let projections = vec![nested; 256].join(", ");
    assert!(parse_cypher(&format!("RETURN {projections}")).is_ok());
}

#[test]
fn expression_depth_boundary_is_consistent() {
    for (prefix, suffix) in [("[", "]"), ("(", ")"), ("NOT ", "")] {
        let accepted = format!("RETURN {}0{}", prefix.repeat(63), suffix.repeat(63));
        assert!(parse_cypher(&accepted).is_ok());
        let rejected = format!("RETURN {}0{}", prefix.repeat(64), suffix.repeat(64));
        assert!(matches!(
            parse_cypher(&rejected),
            Err(uqa_graph::cypher::ParseError::ExpressionTooDeep { limit: 64, .. })
        ));
    }
    assert!(parse_cypher(&format!("RETURN 0{}", " + 0".repeat(63))).is_ok());
    assert!(matches!(
        parse_cypher(&format!("RETURN 0{}", " + 0".repeat(64))),
        Err(uqa_graph::cypher::ParseError::ExpressionTooDeep { limit: 64, .. })
    ));
}
