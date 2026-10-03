//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::split_identifier_list;

fn split(text: &str) -> Option<Vec<String>> {
    split_identifier_list(text, b',')
}

#[test]
fn lists_split_as_split_identifier_string_does() {
    assert_eq!(split("").unwrap(), Vec::<String>::new());
    assert_eq!(split("   ").unwrap(), Vec::<String>::new());
    assert_eq!(split("\"$user\", public").unwrap(), ["$user", "public"]);
    assert_eq!(split("A, \"B\" ,c").unwrap(), ["a", "B", "c"]);
    assert_eq!(split("\"a,b\"").unwrap(), ["a,b"]);
    assert_eq!(split("\"a\"\"b\"").unwrap(), ["a\"b"]);
    assert_eq!(split("\"\"").unwrap(), [""]);
    assert_eq!(split(&"x".repeat(70)).unwrap(), ["x".repeat(63)]);
}

#[test]
fn invalid_list_syntax_is_rejected() {
    for text in ["a,,b", "a,", ",a", "\"a", "a b", "\"a\"b"] {
        assert_eq!(split(text), None, "{text:?}");
    }
}
