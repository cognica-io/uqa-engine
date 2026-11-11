//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::key_value::catalog::{decode_catalog_relation_key, relation_key, StoredCatalogIndex};
use crate::key_value::codec::single_str_key;

fn equal(left: &CatalogIndexRow, right: &CatalogIndexRow) {
    assert_eq!(left.relation, right.relation);
    assert_eq!(left.index_type, right.index_type);
    assert_eq!(left.table_name, right.table_name);
    assert_eq!(left.columns_json, right.columns_json);
    assert_eq!(left.parameters_json, right.parameters_json);
    assert_eq!(left.definition_json, right.definition_json);
}

#[test]
fn catalog_index_input_preserves_stored_struct_and_legacy_name_forms() {
    let payloads = [
        r#"{"index_type":"btree","table_name":"public.t","columns_json":"[ \"a\" ]","parameters_json":"{ \"x\": 1.00 }"}"#,
        r#"{"parameters_json":"{}","columns_json":"[]","table_name":"\"schema.name\".\"Ta\"\"ble\"","index_type":"a\u0000한글","definition_json":"{ \"n\": -0.0 }","ignored":{"a":[1e9999,"\ud800",null]}}"#,
        r#"["btree","public.t","[]","{}",null]"#,
        r#"["btree","public.t","[]","{}"]"#,
        r#"{"index_\u0074ype":"btree","table_name":"public.t","columns_json":"[]","parameters_json":"{}","definition_json":null}"#,
    ];
    let deep = format!(
        r#"{{"index_type":"btree","table_name":"t","columns_json":"[]","parameters_json":"{{}}","ignored":{}0{}}}"#,
        "[".repeat(256),
        "]".repeat(256)
    );
    let keys = [
        relation_key(
            TAG_CATALOG_INDEX,
            &RelationIdentity::new("Quoted.Schema", "Index\"Name"),
        )
        .unwrap(),
        single_str_key(TAG_CATALOG_INDEX, "\"Quoted.Schema\".\"Index\"\"Name\"").unwrap(),
        single_str_key(TAG_CATALOG_INDEX, "legacy").unwrap(),
    ];
    for key in &keys {
        for payload in payloads.into_iter().chain([deep.as_str()]) {
            let original: StoredCatalogIndex = serde_json::from_str(payload).unwrap();
            let (relation, _, _) = decode_catalog_relation_key(key).unwrap();
            let original = CatalogIndexRow {
                relation,
                index_type: original.index_type,
                table_name: original.table_name,
                columns_json: original.columns_json,
                parameters_json: original.parameters_json,
                definition_json: original.definition_json,
            };
            let control = StorageReadControl::with_limit(65536);
            let decoded = decode_index(key, payload.as_bytes(), &control)
                .unwrap()
                .unwrap();
            equal(&decoded, &original);
            drop(decoded);
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn catalog_index_input_rejects_invalid_field_shapes_without_retained_allocations() {
    let key = single_str_key(TAG_CATALOG_INDEX, "index").unwrap();
    for (payload, expected) in [
        (
            r#"{"index_type":"x","index_type":!}"#,
            "duplicate field `index_type`",
        ),
        ("{} 1", "missing field `index_type`"),
    ] {
        assert!(serde_json::from_str::<StoredCatalogIndex>(payload)
            .unwrap_err()
            .to_string()
            .contains(expected));
        let control = StorageReadControl::with_limit(65536);
        assert!(decode_index(&key, payload.as_bytes(), &control)
            .unwrap_err()
            .to_string()
            .contains(expected));
        assert_eq!(control.memory().used(), 0);
    }
    for payload in [
        "null",
        "[]",
        "{}",
        "[1,2,3,4]",
        "[\"x\",\"t\",\"[]\",\"{}\",null,0]",
        r#"{"index_type":"x","index_type":"y","table_name":"t","columns_json":"[]","parameters_json":"{}"}"#,
        r#"{"index_type":"x","table_name":"t","columns_json":"[]","parameters_json":null}"#,
        r#"{"index_type":"x","table_name":"t","columns_json":"[]","parameters_json":"{}","definition_json":false}"#,
        r#"{"index_type":"x","table_name":"t","columns_json":"[]","parameters_json":"{}","definition_json":null,"definition_json":null}"#,
        r#"{"index_type":"x","table_name":"t","columns_json":"[]","parameters_json":"\ud800"}"#,
        r#"{"index_type":"x","table_name":"t","columns_json":"[]","parameters_json":"{}"} 1"#,
    ] {
        assert!(
            serde_json::from_str::<StoredCatalogIndex>(payload).is_err(),
            "{payload}"
        );
        let control = StorageReadControl::with_limit(65536);
        assert!(
            matches!(
                decode_index(&key, payload.as_bytes(), &control),
                Err(StorageBackendError::Serde(_))
            ),
            "{payload}"
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn catalog_index_input_accounts_original_buffers_and_retires_denied_construction() {
    let key = single_str_key(TAG_CATALOG_INDEX, "\"S\".\"I\"").unwrap();
    let payload = serde_json::to_vec(&StoredCatalogIndex {
        index_type: "btree".into(),
        table_name: "\"S\".\"T\"".into(),
        columns_json: "[ \"field\" ]".into(),
        parameters_json: " { \"n\": 1.00 } ".into(),
        definition_json: Some("a\n한글".repeat(4096)),
    })
    .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let mut row = None;
    let measured = allocation_counter::measure(|| {
        row = Some(decode_index(&key, &payload, &control).unwrap().unwrap());
    });
    assert_eq!(
        measured.bytes_current,
        i64::try_from(control.memory().used()).unwrap()
    );
    drop(row);
    assert_eq!(control.memory().used(), 0);
    for limit in [0, 1, 32, 128, 1024, 8192] {
        let limited = StorageReadControl::with_limit(limit);
        assert!(matches!(
            decode_index(&key, &payload, &limited),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(limited.memory().used(), 0);
    }
    control.cancellation().cancel();
    assert!(matches!(
        decode_index(&key, &payload, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
