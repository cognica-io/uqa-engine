//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn numeric_session_offsets_use_sql_signs_and_canonical_names() {
    let definition = super::super::catalog::find_parameter("TimeZone").unwrap();
    for (input, expected) in [
        ("5.5", "<+05:30>-05:30"),
        ("-3.5", "<-03:30>+03:30"),
        ("0", "<+00>-00"),
        ("UTC-09:30", "UTC-09:30"),
        ("asia/seoul", "Asia/Seoul"),
    ] {
        assert_eq!(
            parse_setting(definition, input).unwrap(),
            expected,
            "{input}"
        );
    }
    for input in ["Not/A_Zone", "", "EDT", "UTC+999"] {
        let error = parse_setting(definition, input).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22023"));
        assert_eq!(
            error.to_string(),
            format!("invalid value for parameter \"TimeZone\": \"{input}\"")
        );
        assert_eq!(error.detail(), None);
    }
    assert_eq!(
        parse_setting(definition, "1000").unwrap_err().detail(),
        Some("UTC timezone offset is out of range.")
    );
}
