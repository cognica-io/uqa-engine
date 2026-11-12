//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::Expr, catalog::domain::StoredDomain};
use uqa_core::Value;

fn fixture() -> String {
    serde_json::json!({"object_id":vec![7u8;16],"oid":65536,"array_oid":65537,"identity":{"schema":"s","name":"d"},"owner":owner(),"definition":{"name":"s.d","base":"Integer","collation":"old.C","collation_oid":950,"default":Expr::Literal(Value::Int(17)),"not_null":{"name":"nn","catalog_identity":{"object_id":vec![8u8;16],"oid":65538}},"checks":[{"name":"positive","catalog_identity":{"object_id":vec![9u8;16],"oid":65539},"expression":Expr::Column("value".into()),"validated":false}]},"array_name":"_d","usage_acl":[{"role":null,"grantor":owner(),"grant_option":true}]}).to_string()
}

// Field equivalence does not cancel; resource probes use their original input tokens directly below.
fn read(text: &str, control: &ProductionControl<'_>) -> Result<Produced<StoredDomain>, SQLError> {
    let token = CancellationToken::new();
    StoredDomain::from_stored_schema_json(text, control.budget().unwrap(), &token, &token)
}

fn path_mut<'a>(mut value: &'a mut serde_json::Value, path: &[&str]) -> &'a mut serde_json::Value {
    for key in path {
        value = if *key == "0" {
            &mut value[0]
        } else {
            &mut value[*key]
        };
    }
    value
}

#[test]
fn stored_domain_input_preserves_optional_fields_defaults_and_nested_constraint_records() {
    let text = fixture();
    equivalent(&text, read);
    mutations(&text, read);
    let original: serde_json::Value = serde_json::from_str(&text).unwrap();
    for path in [
        &["definition"][..],
        &["definition", "not_null"][..],
        &["definition", "checks", "0"][..],
    ] {
        let mut current = original.clone();
        let fields = path_mut(&mut current, path)
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for field in fields {
            for replacement in [None, Some(serde_json::Value::Null)] {
                let mut value = original.clone();
                let target = path_mut(&mut value, path).as_object_mut().unwrap();
                if let Some(replacement) = replacement {
                    target.insert(field.clone(), replacement);
                } else {
                    target.remove(&field);
                }
                equivalent(&value.to_string(), read);
            }
        }
        let mut value = original.clone();
        path_mut(&mut value, path)["unknown"] = serde_json::json!({"ignored":[1,2,"한글"]});
        equivalent(&value.to_string(), read);
    }
}

#[test]
fn stored_domain_input_preserves_positional_requirements_and_duplicate_optional_fields() {
    let original: serde_json::Value = serde_json::from_str(&fixture()).unwrap();
    for (path, fields) in [
        (
            &[][..],
            &[
                "object_id",
                "oid",
                "array_oid",
                "identity",
                "owner",
                "definition",
                "array_name",
                "usage_acl",
            ][..],
        ),
        (
            &["definition"][..],
            &[
                "name",
                "base",
                "collation",
                "collation_oid",
                "default",
                "not_null",
                "checks",
            ][..],
        ),
        (
            &["definition", "not_null"][..],
            &["name", "catalog_identity"][..],
        ),
        (
            &["definition", "checks", "0"][..],
            &["name", "catalog_identity", "expression", "validated"][..],
        ),
    ] {
        let mut copy = original.clone();
        let object = path_mut(&mut copy, path);
        let mut values = fields
            .iter()
            .map(|key| object[*key].clone())
            .collect::<Vec<_>>();
        values.push(serde_json::Value::Null);
        for end in 0..=values.len() {
            let mut value = original.clone();
            *path_mut(&mut value, path) = serde_json::to_value(&values[..end]).unwrap();
            equivalent(&value.to_string(), read);
        }
    }
    for (field, replacement) in [
        ("array_oid", "null"),
        ("default", "null"),
        ("collation", "null"),
        ("catalog_identity", "null"),
        ("validated", "true"),
    ] {
        equivalent(
            &fixture().replacen(
                &format!("\"{field}\":"),
                &format!("\"{field}\":{replacement},\"{field}\":"),
                1,
            ),
            read,
        );
    }
}

#[test]
fn stored_domain_input_preserves_scalar_expression_depth_numeric_boundaries_and_ignored_values() {
    for number in [
        "-0",
        "-1",
        "0",
        "1.0",
        "1e0",
        "4294967296",
        "9223372036854775808",
    ] {
        equivalent(&fixture().replace("65536", number), read);
        equivalent(&fixture().replace("65538", number), read);
    }
    for depth in [1, 16, 62, 63, 124, 125, 126, 127] {
        let mut value: serde_json::Value = serde_json::from_str(&fixture()).unwrap();
        value["definition"]["default"] = "placeholder".into();
        let expr = format!(
            "{}{}{}",
            "{\"Not\":".repeat(depth),
            serde_json::to_string(&Expr::Literal(Value::Bool(true))).unwrap(),
            "}".repeat(depth)
        );
        equivalent(&value.to_string().replace("\"placeholder\"", &expr), read);
        value["definition"]["default"] = serde_json::Value::Null;
        value["ignored"] = "placeholder".into();
        equivalent(
            &value.to_string().replace(
                "\"placeholder\"",
                &format!("{}0{}", "[".repeat(depth), "]".repeat(depth)),
            ),
            read,
        );
    }
    equivalent(&fixture().replace("\"old.C\"", "\"bad\\q\""), read);
}

#[test]
fn stored_domain_input_admits_native_expression_buffers_and_retires_both_cancellation_owners() {
    let token = CancellationToken::new();
    retention(&fixture(), |text, control| {
        StoredDomain::from_stored_schema_json(text, control.budget().unwrap(), &token, &token)
    });
    let text = fixture().replace("positive", &"유형".repeat(4096));
    for original_owner in [true, false] {
        let memory = MemoryBudget::new(4 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let selected = if original_owner { &original } else { &invoking };
        let result = crate::ast::stored_input::test_control::cancel_after_allocation(
            &memory,
            selected,
            4096,
            || StoredDomain::from_stored_schema_json(&text, &memory, &original, &invoking),
        );
        assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}
