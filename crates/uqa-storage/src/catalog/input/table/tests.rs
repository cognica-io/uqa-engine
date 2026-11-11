//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn catalog_table_field_input_preserves_original_sequence_shapes_and_spelling() {
    for text in ["[]", r#"["","a","a","한\u0000글","a\nb"]"#] {
        let original: Vec<String> = serde_json::from_str(text).unwrap();
        let control = StorageReadControl::with_limit(65536);
        let restored = field_names(text.as_bytes(), &control).unwrap();
        assert_eq!(*restored, original);
        drop(restored);
        assert_eq!(control.memory().used(), 0);
    }
    let deeply_ignored = format!(
        r#"[{{"unknown":{}[1e9999,"\ud800"]{},"dimensions":0,"field":"x"}}]"#,
        "[".repeat(256),
        "]".repeat(256)
    );
    for text in [
        "[]",
        r#"[{"field":"v","dimensions":0},{"dimensions":4294967295,"field":"v"}]"#,
        r#"[["a",1],["a",2],["\u0000한글",16]]"#,
        r#"[{"fi\u0065ld":"a\nb","dim\u0065nsions":3}]"#,
        &deeply_ignored,
    ] {
        let original: Vec<VectorFieldSchema> = serde_json::from_str(text).unwrap();
        let control = StorageReadControl::with_limit(65536);
        let restored = vector_fields(text.as_bytes(), &control).unwrap();
        assert_eq!(restored.len(), original.len());
        for (restored, original) in restored.iter().zip(&original) {
            assert_eq!(restored.field, original.field);
            assert_eq!(restored.dimensions, original.dimensions);
        }
        assert_eq!(
            serde_json::to_vec(&*restored).unwrap(),
            serde_json::to_vec(&original).unwrap()
        );
        drop(restored);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn catalog_table_field_input_preserves_numeric_and_duplicate_error_order() {
    for text in [
        "null",
        "{}",
        "[1]",
        r#"["\ud800"]"#,
        r#"["a",false]"#,
        r#"["a"] null"#,
    ] {
        assert!(serde_json::from_str::<Vec<String>>(text).is_err(), "{text}");
        let control = StorageReadControl::with_limit(65536);
        assert!(
            matches!(
                field_names(text.as_bytes(), &control),
                Err(StorageBackendError::Serde(_))
            ),
            "{text}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    for text in [
        "null",
        "{}",
        "[null]",
        "[{}]",
        "[[]]",
        r#"[["a"]]"#,
        r#"[["a",1,0]]"#,
        r#"[{"field":1,"dimensions":2}]"#,
        r#"[{"field":"a","dimensions":null}]"#,
        r#"[{"field":"a","dimensions":-0}]"#,
        r#"[{"field":"a","dimensions":-1}]"#,
        r#"[{"field":"a","dimensions":1.0}]"#,
        r#"[{"field":"a","dimensions":1e0}]"#,
        r#"[{"field":"a","dimensions":4294967296}]"#,
        r#"[{"field":"a","dimensions":1e9999}]"#,
        r#"[{"field":"\ud800","dimensions":1}]"#,
        r#"[{"field":"a","field":"b","dimensions":1}]"#,
        r#"[{"field":"a","dimensions":1,"dimensions":2}]"#,
        r#"[{"field":"a","dimensions":1}] 0"#,
    ] {
        assert!(
            serde_json::from_str::<Vec<VectorFieldSchema>>(text).is_err(),
            "{text}"
        );
        let control = StorageReadControl::with_limit(65536);
        assert!(
            matches!(
                vector_fields(text.as_bytes(), &control),
                Err(StorageBackendError::Serde(_))
            ),
            "{text}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    for (text, diagnostic) in [
        (r#"[{"field":"a","field":!"#, "duplicate field `field`"),
        (
            r#"[{"dimensions":1,"dimensions":!"#,
            "duplicate field `dimensions`",
        ),
        ("[{}] !", "missing field `field`"),
        (r#"[{"field":"a"}] !"#, "missing field `dimensions`"),
    ] {
        assert!(serde_json::from_str::<Vec<VectorFieldSchema>>(text)
            .unwrap_err()
            .to_string()
            .contains(diagnostic));
        let control = StorageReadControl::with_limit(65536);
        assert!(vector_fields(text.as_bytes(), &control)
            .unwrap_err()
            .to_string()
            .contains(diagnostic));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn catalog_table_field_input_accounts_destinations_before_copy_and_retains_their_lease() {
    let fields: Vec<String> = (0..257)
        .map(|index| format!("field_{index}_{}", "한\0\n".repeat(17)))
        .collect();
    let vectors: Vec<VectorFieldSchema> = fields
        .iter()
        .map(|field| VectorFieldSchema {
            field: field.clone(),
            dimensions: 32,
        })
        .collect();
    let text = serde_json::to_vec(&fields).unwrap();
    let vector_text = serde_json::to_vec(&vectors).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let sibling = control.memory().reserve(19).unwrap();
    let mut restored = None;
    let measured =
        allocation_counter::measure(|| restored = Some(field_names(&text, &control).unwrap()));
    let restored = restored.unwrap();
    let bytes = restored.capacity() * size_of::<String>()
        + restored.iter().map(String::capacity).sum::<usize>();
    assert_eq!(measured.bytes_current, i64::try_from(bytes).unwrap());
    assert_eq!(restored.reserved_bytes(), bytes);
    assert_eq!(control.memory().used(), bytes + 19);
    assert_eq!(*restored, fields);
    drop(restored);
    let mut restored = None;
    let measured = allocation_counter::measure(|| {
        restored = Some(vector_fields(&vector_text, &control).unwrap());
    });
    let restored = restored.unwrap();
    let bytes = restored.capacity() * size_of::<VectorFieldSchema>()
        + restored
            .iter()
            .map(|field| field.field.capacity())
            .sum::<usize>();
    assert_eq!(measured.bytes_current, i64::try_from(bytes).unwrap());
    assert_eq!(restored.reserved_bytes(), bytes);
    assert_eq!(control.memory().used(), bytes + 19);
    drop(restored);
    assert_eq!(control.memory().used(), 19);
    drop(sibling);
    assert_eq!(control.memory().used(), 0);

    let huge = format!("[\"{}\"]", "x".repeat(4 << 20));
    let limited = StorageReadControl::with_limit(1024);
    let measured = allocation_counter::measure(|| {
        assert!(matches!(
            field_names(huge.as_bytes(), &limited),
            Err(StorageBackendError::Memory(_))
        ));
    });
    assert!(
        measured.bytes_total < 8192,
        "denied name copied: {measured:?}"
    );
    assert_eq!(limited.memory().used(), 0);
}

#[test]
fn catalog_table_field_input_retires_every_truncation_cancellation_and_partial_admission() {
    let fields = br#"["first","escaped\nfield","last"]"#;
    let vectors = br#"[{"field":"first","dimensions":1},["escaped\nfield",2]]"#;
    for length in 0..fields.len() {
        let control = StorageReadControl::with_limit(65536);
        assert!(
            field_names(&fields[..length], &control).is_err(),
            "{length}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    for length in 0..vectors.len() {
        let control = StorageReadControl::with_limit(65536);
        assert!(
            vector_fields(&vectors[..length], &control).is_err(),
            "{length}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    let fields = serde_json::to_vec(&vec!["wide".repeat(1024); 257]).unwrap();
    let vectors = serde_json::to_vec(&vec![
        VectorFieldSchema {
            field: "wide".repeat(1024),
            dimensions: 64
        };
        257
    ])
    .unwrap();
    for limit in [0, 1, 16, 32, 128, 1024, 8192, 65536] {
        let control = StorageReadControl::with_limit(limit);
        assert!(matches!(
            field_names(&fields, &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), 0);
        assert!(matches!(
            vector_fields(&vectors, &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), 0);
    }
    let control = StorageReadControl::with_limit(1 << 24);
    control.cancellation().cancel();
    assert!(matches!(
        field_names(&fields, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        vector_fields(&vectors, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
