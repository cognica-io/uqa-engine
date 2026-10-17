//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::compile_pg_regex;

#[test]
fn postgres_regex_flags_control_expansion_quoting_and_newlines() {
    assert!(compile_pg_regex("a b", "x").unwrap().is_match("ab"));
    assert!(!compile_pg_regex("a b", "x").unwrap().is_match("a b"));
    assert!(compile_pg_regex("a.b", "q").unwrap().is_match("a.b"));
    assert!(compile_pg_regex("a.b", "").unwrap().is_match("a\nb"));
    assert!(!compile_pg_regex("a.b", "n").unwrap().is_match("a\nb"));
    assert!(!compile_pg_regex("[^a]", "n").unwrap().is_match("\n"));
    assert!(compile_pg_regex("[^]a]", "n").unwrap().is_match("b"));
    assert!(!compile_pg_regex("[^]a]", "n").unwrap().is_match("]"));
    assert!(!compile_pg_regex("[^]a]", "n").unwrap().is_match("\n"));
    assert!(!compile_pg_regex(r"[^\n]", "en").unwrap().is_match("\n"));
    for flags in ["m", "n", "p"] {
        assert!(compile_pg_regex("[^-a]", flags).unwrap().is_match("1"));
        assert!(!compile_pg_regex("[^-a]", flags).unwrap().is_match("\n"));
    }
    for flags in ["", "n"] {
        assert!(compile_pg_regex("[[]", flags).unwrap().is_match("["));
        assert!(compile_pg_regex("[^[]", flags).unwrap().is_match("1"));
        assert!(!compile_pg_regex("[^[]", flags).unwrap().is_match("["));
    }
    assert!(compile_pg_regex("[^a]", "s").unwrap().is_match("\n"));
    assert!(compile_pg_regex("[ ]", "x").unwrap().is_match(" "));
    assert!(compile_pg_regex("[[:digit:] ]", "x").unwrap().is_match(" "));
    assert!(compile_pg_regex("[[:digit:]#]", "x").unwrap().is_match("#"));
    assert!(compile_pg_regex("a # ignored\n b", "x")
        .unwrap()
        .is_match("ab"));
    for literal in ['\u{0085}', '\u{00A0}', '\u{2007}', '\u{202F}'] {
        let pattern = format!("a{literal}b");
        assert!(!compile_pg_regex(&pattern, "x").unwrap().is_match("ab"));
        assert!(compile_pg_regex(&pattern, "x").unwrap().is_match(&pattern));
    }
    assert!(compile_pg_regex("a\u{2003}b", "x").unwrap().is_match("ab"));
    for syntax in ["b", "e"] {
        assert!(compile_pg_regex("a+", syntax).unwrap().is_match("a+"));
        assert!(!compile_pg_regex("a+", syntax).unwrap().is_match("aa"));
    }
    assert!(compile_pg_regex(r"a\{1,\}", "b").unwrap().is_match("aa"));
    assert!(compile_pg_regex(r"\d", "b").unwrap().is_match("d"));
    assert!(!compile_pg_regex(r"\d", "b").unwrap().is_match("1"));
    for syntax in ["b", "e"] {
        assert!(compile_pg_regex("a^b", syntax).unwrap().is_match("a^b"));
        assert!(compile_pg_regex("a$b", syntax).unwrap().is_match("a$b"));
        assert!(compile_pg_regex("^ab$", syntax).unwrap().is_match("ab"));
    }
}

#[test]
fn postgres_regex_rejects_invalid_options_with_pg_sqlstate() {
    let error = compile_pg_regex("a", "z").unwrap_err();
    assert_eq!(error.sqlstate(), Some("22023"));
    let error = compile_pg_regex("a", "g").unwrap_err();
    assert_eq!(error.sqlstate(), Some("22023"));
    assert_eq!(
        error.to_string(),
        "regexp_like() does not support the \"global\" option"
    );
    for flags in ["qn", "qp", "qw", "qx"] {
        let error = compile_pg_regex("a", flags).unwrap_err();
        assert_eq!(error.sqlstate(), Some("2201B"));
    }
    assert!(compile_pg_regex("a", "qns").unwrap().is_match("a"));
}
