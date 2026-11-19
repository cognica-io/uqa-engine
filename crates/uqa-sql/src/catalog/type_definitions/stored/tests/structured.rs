//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::ColumnType;

fn range() -> String {
    serde_json::json!({
        "object_id":vec![3u8;16],"oid":20000,"array_oid":20001,"array_name":"_range",
        "identity":{"schema":"public","name":"range"},"owner":owner(),
        "subtype":ColumnType::Array(Box::new(ColumnType::Varchar(Some(32)))),
        "subtype_operator_class":1978,"collation_oid":0,"canonical_oid":null,"subtype_difference_oid":20005,
        "multirange":{"object_id":vec![4u8;16],"oid":20002,"array_oid":20003,"array_name":"_multi","identity":{"schema":"other","name":"multi"}},
        "usage_acl":[{"role":null,"grantor":owner(),"grant_option":false}],
    }).to_string()
}
fn composite() -> String {
    serde_json::json!({
        "object_id":vec![5u8;16],"oid":21000,"relation_oid":21001,"array_oid":21002,"array_name":"_pair",
        "identity":{"schema":"public","name":"pair"},"owner":owner(),
        "attributes":[
            {"name":"alive","ty":ColumnType::Array(Box::new(ColumnType::Varchar(Some(32)))),"collation":"pg_catalog.\"C\"","collation_oid":950,"number":1},
            {"name":"dropped","ty":"Text","number":2,"dropped":true},
        ],"usage_acl":[],
    }).to_string()
}
fn sequences<T: serde::de::DeserializeOwned + serde::Serialize + std::fmt::Debug>(
    text: &str,
    fields: &[&str],
    read: Read<T>,
) {
    let value: serde_json::Value = serde_json::from_str(text).unwrap();
    let mut fields = fields
        .iter()
        .map(|field| value[*field].clone())
        .collect::<Vec<_>>();
    for end in 0..=fields.len() {
        equivalent(&serde_json::to_string(&fields[..end]).unwrap(), read);
    }
    fields.push(serde_json::Value::Null);
    equivalent(&serde_json::to_string(&fields).unwrap(), read);
}

#[test]
fn stored_range_input_preserves_optional_support_and_multirange_identity_shapes() {
    let read = StoredRange::from_stored_json_with_control;
    let text = range();
    equivalent(&text, read);
    mutations(&text, read);
    sequences(
        &text,
        &[
            "object_id",
            "oid",
            "array_oid",
            "array_name",
            "identity",
            "owner",
            "subtype",
            "subtype_operator_class",
            "collation_oid",
            "canonical_oid",
            "subtype_difference_oid",
            "multirange",
            "usage_acl",
        ],
        read,
    );
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    for field in ["object_id", "oid", "array_oid", "array_name", "identity"] {
        let mut changed = value.clone();
        changed["multirange"].as_object_mut().unwrap().remove(field);
        equivalent(&changed.to_string(), read);
    }
    let mut changed = value;
    changed["multirange"]["unknown"] = true.into();
    equivalent(&changed.to_string(), read);
}

#[test]
fn stored_composite_input_preserves_attribute_positions_defaults_and_closed_fields() {
    let read = StoredComposite::from_stored_json_with_control;
    let text = composite();
    equivalent(&text, read);
    mutations(&text, read);
    sequences(
        &text,
        &[
            "object_id",
            "oid",
            "relation_oid",
            "array_oid",
            "array_name",
            "identity",
            "owner",
            "attributes",
            "usage_acl",
        ],
        read,
    );
    let original: serde_json::Value = serde_json::from_str(&text).unwrap();
    for field in [
        "name",
        "ty",
        "collation",
        "collation_oid",
        "number",
        "dropped",
    ] {
        let mut value = original.clone();
        value["attributes"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
        equivalent(&value.to_string(), read);
    }
    let attribute = &original["attributes"][0];
    let fields = [
        "name",
        "ty",
        "collation",
        "collation_oid",
        "number",
        "dropped",
    ]
    .map(|field| attribute[field].clone());
    for end in 0..=fields.len() {
        let mut value = original.clone();
        value["attributes"][0] = serde_json::to_value(&fields[..end]).unwrap();
        equivalent(&value.to_string(), read);
    }
    let mut value = original;
    value["attributes"][0]["unknown"] = true.into();
    equivalent(&value.to_string(), read);
}

#[test]
fn stored_structured_type_input_retains_numeric_and_nested_type_failures() {
    for number in [
        "-0",
        "-1",
        "0",
        "1.0",
        "1e0",
        "2147483648",
        "4294967296",
        "999999999999999999999999999999",
    ] {
        equivalent(
            &range().replace("20000", number),
            StoredRange::from_stored_json_with_control,
        );
        equivalent(
            &composite().replace("\"number\":1", &format!("\"number\":{number}")),
            StoredComposite::from_stored_json_with_control,
        );
    }
    for depth in [15, 16, 62, 63, 124, 125, 126, 127] {
        let ty = format!(
            "{}\"Integer\"{}",
            "{\"Array\":".repeat(depth),
            "}".repeat(depth)
        );
        let text = range();
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        value["subtype"] = "placeholder".into();
        equivalent(
            &value.to_string().replace("\"placeholder\"", &ty),
            StoredRange::from_stored_json_with_control,
        );
    }
}

#[test]
fn stored_structured_type_input_admits_owned_buffers_and_both_cancellation_owners() {
    retention(&range(), StoredRange::from_stored_json_with_control);
    retention(&composite(), StoredComposite::from_stored_json_with_control);
    cancellation(
        &range().replace("_range", &"유형".repeat(4096)),
        StoredRange::from_stored_json_with_control,
    );
    cancellation(
        &composite().replace("_pair", &"유형".repeat(4096)),
        StoredComposite::from_stored_json_with_control,
    );
}
