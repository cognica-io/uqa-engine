//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{check_assignable_custom_name, valid_custom_name};
use crate::SQLError;

#[test]
fn custom_names_are_two_or_more_simple_identifiers() {
    for name in ["my.var", "a.b.c", "a.b$", "_x.y1", "My.Var"] {
        assert!(valid_custom_name(name), "{name:?}");
    }
    for name in [
        "my", "1a.b", "a.1b", "a. b", "a..b", ".a", "a.", "a.b-c", "a.$b",
    ] {
        assert!(!valid_custom_name(name), "{name:?}");
    }
}

#[test]
fn assignable_names_report_postgres_errors() {
    assert!(check_assignable_custom_name("my.var", []).is_ok());
    assert_eq!(
        check_assignable_custom_name("my", [])
            .unwrap_err()
            .to_string(),
        "unrecognized configuration parameter \"my\""
    );
    let SQLError::Diagnostic {
        sqlstate, detail, ..
    } = check_assignable_custom_name("1a.b", []).unwrap_err()
    else {
        panic!("diagnostic expected");
    };
    assert_eq!(sqlstate, "42602");
    assert_eq!(
        detail.as_deref(),
        Some("Custom parameter names must be two or more simple identifiers separated by dots.")
    );
    let SQLError::Diagnostic {
        message, detail, ..
    } = check_assignable_custom_name("plpgsql.nosuch", ["plpgsql"]).unwrap_err()
    else {
        panic!("diagnostic expected");
    };
    assert_eq!(
        message,
        "invalid configuration parameter name \"plpgsql.nosuch\""
    );
    assert_eq!(detail.as_deref(), Some("\"plpgsql\" is a reserved prefix."));
    assert!(check_assignable_custom_name("plpgsqlx.a", ["plpgsql"]).is_ok());
}
