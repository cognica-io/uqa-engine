//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn equivalent(text: &[u8]) {
    let expected = serde_json::from_slice::<StoredSequence>(text);
    let control = StorageReadControl::with_limit(1 << 20);
    match (expected, read(text, &control)) {
        (Ok(expected), Ok(actual)) => assert_eq!(
            serde_json::to_value(&*actual).unwrap(),
            serde_json::to_value(expected).unwrap(),
            "{text:?}"
        ),
        (Err(_), Err(_)) => (),
        (expected, actual) => panic!("{text:?}: {expected:?} / {actual:?}"),
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn sequence_definition_input_matches_original_defaults_flattening_and_options() {
    let owner = serde_json::to_string(&uqa_core::catalog_role::RoleIdentity::BOOTSTRAP).unwrap();
    for fields in [
        String::new(),
        r#", "current":1, "called":false, "log_count":0"#.into(),
        r#", "options":{"data_type":"bigint","cycle":false}"#.into(),
        r#", "options":["integer",null,null,true,7]"#.into(),
        r#", "options":{"data_type":"integer","cycle":false,"cycle":true}"#.into(),
        r#", "acl":[{"role":"reader한글","privileges":{"select":true,"update":true,"usage":true}}]"#.into(),
        r#", "role_owner":false,"role_owner":"uqa","acl":false,"acl":null"#.into(),
        format!(r#", "sequence_security_format":1,"role_owner":{owner},"acl":null"#),
        format!(r#", "sequence_security_format":1,"role_owner":{owner}"#),
        r#", "sequence_security_format":1.0,"role_owner":"uqa","acl":null"#.into(),
        r#", "unexpected":false"#.into(),
        r#", "start":2"#.into(),
        r#", "owner":null,"persistence":"u""#.into(),
        r#", "current":null,"called":null,"log_count":null"#.into(),
    ] { equivalent(format!(r#"{{"start":1,"increment":1{fields}}}"#).as_bytes()); }
    for number in [
        "-0",
        "1.0",
        "1e0",
        "9223372036854775808",
        "-9223372036854775809",
        "null",
        "true",
        "-1",
    ] {
        equivalent(format!(r#"{{"start":{number},"increment":1}}"#).as_bytes());
    }
}

#[test]
fn sequence_definition_input_rejects_original_malformed_prefixes_and_releases_buffers() {
    let text = r#"{"start":1,"increment":1,"role_owner":"owner한글","options":{"data_type":"bigint","cycle":false},"acl":[{"role":"reader","grantor":"owner"}]}"#;
    for end in 0..text.len() {
        equivalent(&text.as_bytes()[..end]);
    }
    for position in 0..text.len() {
        for replacement in [b'0', b'"', b'[', b'}', 0xff] {
            let mut bytes = text.as_bytes().to_vec();
            bytes[position] = replacement;
            equivalent(&bytes);
        }
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let row = read(text.as_bytes(), &control).unwrap();
    assert_eq!(row.reserved_bytes(), control.memory().used());
    let peak = control.memory().peak();
    control.cancellation().cancel();
    assert!(read(text.as_bytes(), &control).is_err());
    assert_eq!(row.persistence, "p");
    drop(row);
    assert_eq!(control.memory().used(), 0);
    for limit in (0..peak).step_by(31).chain([peak - 1]) {
        let control = StorageReadControl::with_limit(limit);
        assert!(read(text.as_bytes(), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn sequence_value_input_checks_every_original_allocation_field() {
    let text = r#"{"current":4,"called":true,"log_count":2,"increment":1,"min_value":1,"max_value":100,"cycle":false,"cache_size":1}"#;
    for original in [
        text.to_owned(),
        text.replace("\"cache_size\":1", "\"cache_size\":null"),
        text.replace("\"cycle\":false", "\"cycle\":0"),
        text.replace("\"increment\":1,", ""),
        text.replace("\"current\":4", "\"current\":4,\"current\":5"),
    ] {
        let control = StorageReadControl::with_limit(65536);
        match (
            serde_json::from_str::<StoredSequenceValue>(&original),
            value(original.as_bytes(), &control),
        ) {
            (Ok(expected), Ok(actual)) => assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(expected).unwrap()
            ),
            (Err(_), Err(_)) => (),
            (expected, actual) => panic!("{original}: {expected:?} / {actual:?}"),
        }
        assert_eq!(control.memory().used(), 0);
    }
}
