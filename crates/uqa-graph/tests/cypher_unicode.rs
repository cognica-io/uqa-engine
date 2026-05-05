//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cypher string-literal UTF-8 fidelity. The lexer used to iterate
//! source bytes and cast each to `char`, which silently corrupted
//! every non-ASCII codepoint in a string literal. These regression
//! tests pin the proper UTF-8 round-trip.

use uqa_graph::cypher::parse_cypher;

fn parsed_string_value(query: &str) -> String {
    let q = parse_cypher(query).expect("parses");
    let dbg = format!("{q:?}");
    dbg
}

#[test]
fn single_quoted_latin_extended_round_trips() {
    let dbg = parsed_string_value("RETURN 'café' AS x");
    assert!(dbg.contains("café"), "lost UTF-8: {dbg}");
}

#[test]
fn double_quoted_cjk_round_trips() {
    let dbg = parsed_string_value("RETURN \"안녕하세요\" AS greeting");
    assert!(dbg.contains("안녕하세요"), "lost CJK: {dbg}");
}

#[test]
fn emoji_round_trips() {
    let dbg = parsed_string_value("RETURN '🚀 lift off' AS msg");
    assert!(dbg.contains("🚀 lift off"), "lost emoji: {dbg}");
}

#[test]
fn doubled_quote_inside_single_quote_still_works() {
    let dbg = parsed_string_value("RETURN 'it''s café' AS x");
    assert!(dbg.contains("it's café"), "doubled quote + UTF-8: {dbg}");
}

#[test]
fn escape_sequence_inside_utf8_string_still_works() {
    // Source has the two chars '\\' then 'n', which the lexer maps to
    // a real newline. Rust's debug formatter then escapes that newline
    // back to '\\n' for display, so the substring we look for is the
    // two-char escape — same content, different layer.
    let dbg = parsed_string_value(r"RETURN 'café\nbar' AS x");
    assert!(dbg.contains(r"café\nbar"), "escape + UTF-8: {dbg}");
}

#[test]
fn pure_ascii_strings_unchanged() {
    let dbg = parsed_string_value("RETURN 'hello world' AS x");
    assert!(dbg.contains("hello world"), "ASCII regression: {dbg}");
}
