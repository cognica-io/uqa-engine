//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn deferred_diagnostic_preserves_primary_fields_and_old_serialization() {
    let original = crate::plpgsql::runtime_diagnostics::unassigned_record("r");
    let deferred = DeferredSQLError::from(&original);
    let encoded = serde_json::to_string(&deferred).unwrap();
    let restored: SQLError = serde_json::from_str::<DeferredSQLError>(&encoded)
        .unwrap()
        .into();
    assert_eq!(restored.sqlstate(), original.sqlstate());
    assert_eq!(restored.to_string(), original.to_string());
    assert_eq!(restored.detail(), original.detail());
    assert_eq!(restored.hint(), original.hint());

    let hint_only = SQLError::Diagnostic {
        sqlstate: "42804".into(),
        message: "mismatched parameter".into(),
        detail: None,
        hint: Some("Use the declared type.".into()),
    };
    let restored: SQLError = DeferredSQLError::from(&hint_only).into();
    assert_eq!(restored.hint(), hint_only.hint());
    assert_eq!(restored.detail(), None);

    let legacy = r#"{"sqlstate":"42601","message":"conflicting or redundant options"}"#;
    let restored: DeferredSQLError = serde_json::from_str(legacy).unwrap();
    assert_eq!(serde_json::to_string(&restored).unwrap(), legacy);
    assert!(matches!(SQLError::from(restored), SQLError::Routine { .. }));
}
