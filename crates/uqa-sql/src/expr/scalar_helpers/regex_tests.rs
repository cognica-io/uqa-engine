//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::compile_pg_regex;

#[test]
fn postgres_regex_flags_control_expansion_quoting_and_newlines() {
    assert!(compile_pg_regex("a b", "x", false).unwrap().is_match("ab"));
    assert!(!compile_pg_regex("a b", "x", false).unwrap().is_match("a b"));
    assert!(compile_pg_regex("a.b", "q", false).unwrap().is_match("a.b"));
    assert!(compile_pg_regex("a.b", "", false).unwrap().is_match("a\nb"));
    assert!(!compile_pg_regex("a.b", "n", false)
        .unwrap()
        .is_match("a\nb"));
    assert!(!compile_pg_regex("[^a]", "n", false).unwrap().is_match("\n"));
    assert!(compile_pg_regex("[^]a]", "n", false).unwrap().is_match("b"));
    assert!(!compile_pg_regex("[^]a]", "n", false).unwrap().is_match("]"));
    assert!(!compile_pg_regex("[^]a]", "n", false)
        .unwrap()
        .is_match("\n"));
    assert!(!compile_pg_regex(r"[^\n]", "en", false)
        .unwrap()
        .is_match("\n"));
    for flags in ["m", "n", "p"] {
        assert!(compile_pg_regex("[^-a]", flags, false)
            .unwrap()
            .is_match("1"));
        assert!(!compile_pg_regex("[^-a]", flags, false)
            .unwrap()
            .is_match("\n"));
    }
    for flags in ["", "n"] {
        assert!(compile_pg_regex("[[]", flags, false).unwrap().is_match("["));
        assert!(compile_pg_regex("[^[]", flags, false)
            .unwrap()
            .is_match("1"));
        assert!(!compile_pg_regex("[^[]", flags, false)
            .unwrap()
            .is_match("["));
    }
    assert!(compile_pg_regex("[^a]", "s", false).unwrap().is_match("\n"));
    assert!(compile_pg_regex("[ ]", "x", false).unwrap().is_match(" "));
    assert!(compile_pg_regex("[[:digit:] ]", "x", false)
        .unwrap()
        .is_match(" "));
    assert!(compile_pg_regex("[[:digit:]#]", "x", false)
        .unwrap()
        .is_match("#"));
    assert!(compile_pg_regex("a # ignored\n b", "x", false)
        .unwrap()
        .is_match("ab"));
    for literal in ['\u{0085}', '\u{00A0}', '\u{2007}', '\u{202F}'] {
        let pattern = format!("a{literal}b");
        assert!(!compile_pg_regex(&pattern, "x", false)
            .unwrap()
            .is_match("ab"));
        assert!(compile_pg_regex(&pattern, "x", false)
            .unwrap()
            .is_match(&pattern));
    }
    assert!(compile_pg_regex("a\u{2003}b", "x", false)
        .unwrap()
        .is_match("ab"));
    for syntax in ["b", "e"] {
        assert!(compile_pg_regex("a+", syntax, false)
            .unwrap()
            .is_match("a+"));
        assert!(!compile_pg_regex("a+", syntax, false)
            .unwrap()
            .is_match("aa"));
    }
    assert!(compile_pg_regex(r"a\{1,\}", "b", false)
        .unwrap()
        .is_match("aa"));
    assert!(compile_pg_regex(r"\d", "b", false).unwrap().is_match("d"));
    assert!(!compile_pg_regex(r"\d", "b", false).unwrap().is_match("1"));
    for syntax in ["b", "e"] {
        assert!(compile_pg_regex("a^b", syntax, false)
            .unwrap()
            .is_match("a^b"));
        assert!(compile_pg_regex("a$b", syntax, false)
            .unwrap()
            .is_match("a$b"));
        assert!(compile_pg_regex("^ab$", syntax, false)
            .unwrap()
            .is_match("ab"));
    }
}

#[test]
fn postgres_regex_rejects_invalid_options_with_pg_sqlstate() {
    let error = compile_pg_regex("a", "z", false).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22023"));
    let error = compile_pg_regex("a", "g", false).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22023"));
    for flags in ["qn", "qp", "qw", "qx"] {
        let error = compile_pg_regex("a", flags, false).unwrap_err();
        assert_eq!(error.sqlstate(), Some("2201B"));
    }
    assert!(compile_pg_regex("a", "qns", false).unwrap().is_match("a"));
}
