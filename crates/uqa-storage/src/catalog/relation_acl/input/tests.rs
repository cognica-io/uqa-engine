//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    catalog_acl::TablePrivileges,
    catalog_role::{BoundAclEntry, RoleIdentity},
};

fn paths(count: usize) -> Vec<BoundAclEntry<TablePrivileges>> {
    vec![
        BoundAclEntry {
            role: None,
            grantor: RoleIdentity::BOOTSTRAP,
            privileges: TablePrivileges::ALL,
            grant_options: TablePrivileges::default(),
        };
        count
    ]
}

fn revision() -> String {
    serde_json::to_string(&[7_u8; 16]).unwrap()
}

#[test]
fn catalog_acl_tuple_input_preserves_stored_forms_and_nested_field_order() {
    let revision = revision();
    for acl in [None, Some(Vec::new()), Some(paths(17))] {
        let acl = serde_json::to_string(&acl).unwrap();
        let forms = [
            format!(r#"{{"format":1,"revision":{revision},"acl":{acl}}}"#),
            format!(r#"{{"acl":{acl},"revision":{revision},"format":1}}"#),
            format!(r#"{{"revi\u0073ion":{revision},"format":1,"a\u0063l":{acl}}}"#),
            format!("[1,{revision},{acl}]"),
        ];
        for text in forms {
            let expected = RelationAclTuple::decode(text.as_bytes()).unwrap();
            let control = StorageReadControl::with_limit(65536);
            let restored =
                RelationAclTuple::decode_with_control(text.as_bytes(), &control).unwrap();
            assert_eq!(*restored, expected);
            assert_eq!(
                serde_json::to_vec(&*restored).unwrap(),
                serde_json::to_vec(&expected).unwrap()
            );
            drop(restored);
            assert_eq!(control.memory().used(), 0);
        }
    }
    let role = serde_json::to_string(&RoleIdentity::BOOTSTRAP).unwrap();
    let text = format!(
        r#"{{"acl":[{{"role":null,"grantor":{role},"privileges":{{"ignored":["\ud800",1e9999]}},"grant_options":{{}}}}],"revision":{revision},"format":1}}"#
    );
    let expected = RelationAclTuple::decode(text.as_bytes()).unwrap();
    let control = StorageReadControl::with_limit(65536);
    let restored = RelationAclTuple::decode_with_control(text.as_bytes(), &control).unwrap();
    assert_eq!(*restored, expected);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn catalog_acl_tuple_input_preserves_corruption_and_identity_validation_order() {
    let revision = revision();
    let zero = serde_json::to_string(&[0_u8; 16]).unwrap();
    for text in [
        "null".into(),
        "{}".into(),
        "[]".into(),
        format!("[1,{revision}]"),
        format!("[1,{revision},null,0]"),
        format!(r#"{{"format":1,"revision":{revision}}}"#),
        format!(r#"{{"format":1,"revision":{revision},"acl":{{}}}}"#),
        format!(r#"{{"format":1,"revision":{revision},"acl":null,"unknown":0}}"#),
        format!(r#"{{"format":-0,"revision":{revision},"acl":null}}"#),
        format!(r#"{{"format":1.0,"revision":{revision},"acl":null}}"#),
        format!(r#"{{"format":4294967296,"revision":{revision},"acl":null}}"#),
        format!(r#"{{"format":2,"revision":{revision},"acl":null}}"#),
        format!(r#"{{"format":1,"revision":{zero},"acl":null}}"#),
        r#"{"format":1,"revision":[1],"acl":null}"#.into(),
        format!(r#"{{"format":1,"revision":{revision},"acl":null}} 0"#),
    ] {
        assert!(RelationAclTuple::decode(text.as_bytes()).is_err(), "{text}");
        let control = StorageReadControl::with_limit(65536);
        assert!(
            RelationAclTuple::decode_with_control(text.as_bytes(), &control).is_err(),
            "{text}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    for (text, expected) in [
        (
            r#"{"format":1,"format":!"#.to_owned(),
            "duplicate field `format`",
        ),
        (r#"{"acl":[],"acl":!"#.to_owned(), "duplicate field `acl`"),
        (r#"{"unknown":!"#.to_owned(), "unknown field `unknown`"),
        ("{} !".to_owned(), "missing field `format`"),
        (
            format!(r#"{{"revision":{revision},"format":1}} !"#),
            "missing field `acl`",
        ),
    ] {
        assert!(RelationAclTuple::decode(text.as_bytes())
            .unwrap_err()
            .to_string()
            .contains(expected));
        let control = StorageReadControl::with_limit(65536);
        assert!(
            RelationAclTuple::decode_with_control(text.as_bytes(), &control)
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
        assert_eq!(control.memory().used(), 0);
    }
    let incomplete = format!(r#"{{"format":2,"revision":{revision},"acl":null}} !"#);
    assert!(matches!(
        RelationAclTuple::decode(incomplete.as_bytes()),
        Err(StorageBackendError::Serde(_))
    ));
    let control = StorageReadControl::with_limit(65536);
    assert!(matches!(
        RelationAclTuple::decode_with_control(incomplete.as_bytes(), &control),
        Err(StorageBackendError::Serde(_))
    ));
}

#[test]
fn catalog_acl_tuple_input_keeps_original_path_buffers_until_the_tuple_retires() {
    let tuple = RelationAclTuple::new(Some(paths(257))).unwrap();
    let text = serde_json::to_vec(&tuple).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let sibling = control.memory().reserve(37).unwrap();
    let mut restored = None;
    let measured = allocation_counter::measure(|| {
        restored = Some(RelationAclTuple::decode_with_control(&text, &control).unwrap());
    });
    let restored = restored.unwrap();
    assert_eq!(*restored, tuple);
    let bytes =
        restored.acl.as_ref().unwrap().capacity() * size_of::<BoundAclEntry<TablePrivileges>>();
    assert_eq!(measured.bytes_current, i64::try_from(bytes).unwrap());
    assert_eq!(restored.reserved_bytes(), bytes);
    assert_eq!(control.memory().used(), bytes + 37);
    drop(restored);
    assert_eq!(control.memory().used(), 37);
    drop(sibling);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn catalog_acl_tuple_input_retires_truncation_denial_cancellation_and_invalid_revisions() {
    let tuple = RelationAclTuple::new(Some(paths(3))).unwrap();
    let text = serde_json::to_vec(&tuple).unwrap();
    for length in 0..text.len() {
        let control = StorageReadControl::with_limit(65536);
        assert!(RelationAclTuple::decode_with_control(&text[..length], &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let tuple = RelationAclTuple::new(Some(paths(257))).unwrap();
    let text = serde_json::to_vec(&tuple).unwrap();
    for limit in [0, 1, 32, 128, 1024, 4096, 8192] {
        let control = StorageReadControl::with_limit(limit);
        assert!(matches!(
            RelationAclTuple::decode_with_control(&text, &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), 0);
    }
    let mut invalid = serde_json::to_value(&tuple).unwrap();
    invalid["revision"] = serde_json::to_value([0_u8; 16]).unwrap();
    let invalid = serde_json::to_vec(&invalid).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    assert!(matches!(
        RelationAclTuple::decode_with_control(&invalid, &control),
        Err(StorageBackendError::Other(_))
    ));
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        RelationAclTuple::decode_with_control(&text, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
