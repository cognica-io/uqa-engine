//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn schema_records_distinguish_legacy_names_from_current_role_references() {
    let old = SchemaRow::legacy("public");
    let current = SchemaRow::bootstrap("public");
    for row in [&old, &current] {
        let encoded = serde_json::to_string(row).unwrap();
        assert_eq!(serde_json::from_str::<SchemaRow>(&encoded).unwrap(), *row);
    }
    let encoded = serde_json::to_value(&current).unwrap();
    assert_eq!(encoded["schema_security_format"], 1);
    assert_eq!(encoded["role_owner"]["oid"], 10);
    assert!(encoded["acl"][1]["role"].is_null());
    assert!(serde_json::from_value::<LegacySchemaRow>(encoded).is_err());
    assert_eq!(
        serde_json::from_str::<SchemaRow>(r#"{"name":"ordinary"}"#).unwrap(),
        SchemaRow::legacy("ordinary")
    );
}

#[test]
fn malformed_or_unknown_schema_records_never_fall_back_to_legacy_defaults() {
    let original = serde_json::to_value(SchemaRow::bootstrap("public")).unwrap();
    for version in [
        serde_json::Value::Null,
        serde_json::json!(2),
        serde_json::json!("1"),
    ] {
        let mut value = original.clone();
        value["schema_security_format"] = version;
        value["role_owner"] = serde_json::json!("uqa");
        value["acl"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<SchemaRow>(value).is_err());
    }
    let mut missing_owner = original.clone();
    missing_owner.as_object_mut().unwrap().remove("role_owner");
    assert!(serde_json::from_value::<SchemaRow>(missing_owner).is_err());
    let mut mixed = original;
    mixed["acl"][0]["role"] = serde_json::json!("uqa");
    assert!(serde_json::from_value::<SchemaRow>(mixed).is_err());
}
