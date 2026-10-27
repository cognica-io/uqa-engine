//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{RelationIdentity, RelationName};

#[test]
fn borrowed_references_preserve_decoded_names_and_schema_boundaries() {
    for (reference, schema, name) in [
        ("items", None, "items"),
        ("public.items", Some("public"), "items"),
        ("\"a.b\"", None, "a.b"),
        (
            "\"quoted schema\".\"a\"\"b\"",
            Some("quoted schema"),
            "a\"b",
        ),
        ("\"한글\".\"名.前\"", Some("한글"), "名.前"),
        ("\"\"\"\"", None, "\""),
        ("MixedCase", None, "MixedCase"),
    ] {
        let borrowed = RelationIdentity::borrowed_reference(reference).unwrap();
        assert_eq!(
            borrowed.schema.map(|part| part.to_string()).as_deref(),
            schema
        );
        assert_eq!(borrowed.name.to_string(), name);
        assert_eq!(borrowed.name, RelationName::decoded(name));
        assert_eq!(
            RelationIdentity::parse_reference(reference).unwrap(),
            (schema.map(str::to_owned), name.to_owned())
        );
    }
}

#[test]
fn invalid_references_keep_the_canonical_diagnostic() {
    for reference in [
        "", ".", "a.", ".a", "a..b", "a.b.c", "\"\"", "\"a", "a\"b", "\"a\"b",
    ] {
        assert!(RelationIdentity::borrowed_reference(reference).is_none());
        let expected = match reference {
            "" => "persisted relation name is empty".into(),
            "\"a" => format!("unterminated quoted relation name `{reference}`"),
            _ => format!("invalid persisted relation name `{reference}`"),
        };
        assert_eq!(
            RelationIdentity::parse_reference(reference).unwrap_err(),
            expected
        );
    }
}

#[test]
fn borrowed_comparisons_decode_quotes_without_allocating() {
    let names = ["\"a\"\"b\"", "\"a.b\"", "\"z\"", "\"名\""];
    let observation = allocation_counter::measure(|| {
        let keys = names.map(|name| RelationIdentity::borrowed_reference(name).unwrap().name);
        assert_eq!(keys[0], RelationName::decoded("a\"b"));
        assert_eq!(keys[1], RelationName::decoded("a.b"));
        assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(RelationName::decoded("a") < keys[0]);
    });
    assert_eq!(observation.count_total, 0);
    assert_eq!(observation.bytes_total, 0);
}
