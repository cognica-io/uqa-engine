//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn sequence_value_errors_keep_sqlstate_and_direct_api_diagnostics() {
    let cases = [
        (
            SequenceValueError::Undefined("missing".into()),
            "42P01",
            "relation \"missing\" does not exist",
        ),
        (
            SequenceValueError::WrongKind {
                name: "items".into(),
                kind: "table",
            },
            "42809",
            "cannot open relation \"items\": this operation is not supported for tables",
        ),
        (
            SequenceValueError::CurrvalUndefined("ids".into()),
            "55000",
            "currval of sequence \"ids\" is not yet defined in this session",
        ),
        (
            SequenceValueError::LastvalUndefined,
            "55000",
            "lastval is not yet defined in this session",
        ),
        (
            SequenceValueError::SetvalOutOfBounds {
                name: "ids".into(),
                value: 6,
                min: 1,
                max: 5,
            },
            "22003",
            "setval: value 6 is out of bounds for sequence \"ids\" (1..5)",
        ),
        (
            SequenceValueError::Exhausted {
                name: "ids".into(),
                bound: "minimum",
                value: -5,
            },
            "2200H",
            "nextval: reached minimum value of sequence \"ids\" (-5)",
        ),
        (
            SequenceValueError::ReadOnly("setval"),
            "25006",
            "cannot execute setval() in a read-only transaction",
        ),
    ];
    for (error, expected_code, expected_message) in cases {
        assert_eq!(error.to_string(), expected_message);
        let SQLError::Routine { sqlstate, message } = error.into_sql_error() else {
            panic!("sequence value error must retain its SQL error envelope");
        };
        assert_eq!(sqlstate, expected_code);
        assert_eq!(message, expected_message);
    }
}

#[test]
fn sequence_security_and_internal_errors_keep_their_original_variants() {
    let security = SequenceValueError::from(SQLError::TypeMismatch("security input".into()));
    assert!(
        matches!(security.into_sql_error(), SQLError::TypeMismatch(message) if message == "security input")
    );
    let internal = SequenceValueError::Internal("storage detail".into());
    assert!(
        matches!(internal.into_sql_error(), SQLError::Internal(message) if message == "storage detail")
    );
}
