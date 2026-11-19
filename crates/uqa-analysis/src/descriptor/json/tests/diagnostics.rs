//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use serde::{de, Deserialize, Deserializer};

struct Unique;

impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique)
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Unique, E> {
                Ok(Unique)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Unique, E> {
                Ok(Unique)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Unique, E> {
                Ok(Unique)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Unique, E> {
                Ok(Unique)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Unique, E> {
                Ok(Unique)
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut values: A) -> Result<Unique, A::Error> {
                while values.next_element::<Unique>()?.is_some() {}
                Ok(Unique)
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut values: A) -> Result<Unique, A::Error> {
                let mut names = BTreeSet::new();
                while let Some(name) = values.next_key::<String>()? {
                    if !names.insert(name) {
                        return Err(de::Error::custom("duplicate analyzer descriptor property"));
                    }
                    values.next_value::<Unique>()?;
                }
                Ok(Unique)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

fn reference(text: &str) -> Result<Wire, serde_json::Error> {
    serde_json::from_str::<Unique>(text)?;
    serde_json::from_str(text)
}

fn compare(text: &str) {
    let expected = reference(text);
    let ordinary = inline(decode(text, &mut ()));
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let mut input = decoder(text.into(), &memory, &token);
    let retained = loop {
        match input.advance(&mut NoRead, WINDOW) {
            Ok(None) => (),
            Ok(Some(value)) => break Ok(value),
            Err(error) => break Err(error),
        }
    };
    drop(input);
    for actual in [ordinary, retained] {
        match (&expected, actual) {
            (Ok(expected), Ok(actual)) => assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(expected).unwrap(),
                "{text}"
            ),
            (Err(expected), Err(AnalysisError::JsonDiagnostic(actual))) => {
                assert_eq!(actual.to_string(), expected.to_string(), "{text}");
                assert_eq!(
                    (actual.is_data(), actual.is_syntax(), actual.is_eof()),
                    (expected.is_data(), expected.is_syntax(), expected.is_eof()),
                    "{text}"
                );
                assert_eq!(
                    (actual.position().line, actual.position().column),
                    (expected.line(), expected.column()),
                    "{text}"
                );
            }
            (expected, actual) => panic!(
                "{text}: expected {:?}, actual {:?}",
                expected.as_ref().err(),
                actual.as_ref().err()
            ),
        }
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn descriptor_diagnostics_preserve_original_lexical_and_typed_wire_boundaries() {
    let base = original();
    for text in [
        serde_json::to_string_pretty(&base).unwrap(),
        " {\n\"fingerprint\":\"한🙂\", \"descriptor\":null}".into(),
        "{\"descriptor\":{\"pipeline\":{\"x\":1,\"\\u0078\":2}}}".into(),
        "{\"fingerprint\":false,\"descriptor\":{\"length_policy\":{\"emitted_tokens\":null}}}"
            .into(),
        "[null,\"bad\"]".into(),
    ] {
        for end in (0..=text.len()).filter(|&end| text.is_char_boundary(end)) {
            compare(&text[..end]);
        }
        compare(&format!("{text} trailing"));
    }
    let values = [
        "null",
        "false",
        "-0",
        "-1",
        "1.0",
        "256",
        "4294967296",
        "18446744073709551616",
        "1e400",
        "\"bad\"",
        "[]",
        "{}",
        "[1,2]",
        "[1,2,3,4]",
        "[1,256,3]",
        "[1,-0,3]",
        "[1,2.0,3]",
        "{\"emitted_tokens\":null}",
        "{\"emitted_tokens\":true}",
        "{\"unknown\":false}",
    ];
    for value in values {
        for field in [
            "format",
            "format_version",
            "algorithm_revision",
            "source_mapping_revision",
            "length_policy",
            "pipeline",
            "runtime_profiles",
        ] {
            compare(&format!("{{\"descriptor\":{{\"{field}\":{value}}}}}"));
        }
        for field in ["rust_unicode", "normalization_unicode", "expressions"] {
            compare(&format!(
                "{{\"descriptor\":{{\"runtime_profiles\":{{\"{field}\":{value}}}}}}}"
            ));
        }
        compare(&format!("{{\"fingerprint\":{value}}}"));
    }
    for text in [r#"{"x":1,"x"  :2}"#, "{\n\"x\":1,\n\"x\"  \n :2}"] {
        compare(text);
    }
    let descriptor = serde_json::to_string(&base["descriptor"]).unwrap();
    for fingerprint in ["\"bad\"", "false", "[]", "{}", "\"\"", "null"] {
        for text in [
            format!("[{descriptor},{fingerprint}]"),
            format!("[{descriptor},{fingerprint} ,null]"),
            format!("{{\"descriptor\":{descriptor},\"fingerprint\":{fingerprint} }}"),
            format!("{{\"fingerprint\":{fingerprint} ,\"descriptor\":{descriptor}}}"),
        ] {
            compare(&text);
        }
    }
}

#[test]
fn descriptor_pipeline_preserves_public_generic_scalar_values_and_errors() {
    let keys = uqa_core::json::JsonScalarProtocol::from_values(
        &serde_json::Number::from(1u8),
        serde_json::value::RawValue::NULL,
    )
    .unwrap();
    let mut base = original();
    for key in [keys.number, keys.raw] {
        for payload in [
            "1",
            "-0",
            "1.25",
            "1e99999",
            " 1",
            "01",
            "[]",
            "{}",
            "null",
            "true",
            "{\"x\":1}",
            "[",
            "\"\\ud800\"",
        ] {
            for value in [
                json!({key: payload}),
                json!({key: false}),
                json!({"child": {key: payload}}),
                json!([{key: payload}]),
            ] {
                base["descriptor"]["pipeline"] = value;
                compare(&serde_json::to_string_pretty(&base).unwrap());
            }
        }
    }
}

#[test]
fn descriptor_generic_raw_values_preserve_depth_and_scalar_error_precedence() {
    let keys = uqa_core::json::JsonScalarProtocol::from_values(
        &serde_json::Number::from(1u8),
        serde_json::value::RawValue::NULL,
    )
    .unwrap();
    let mut base = original();
    for depth in [0, 125, 126, 127, 128] {
        for leaf in [
            "null".to_owned(),
            format!("{{{:?}:[]}}", keys.number),
            format!("{{{:?}:false}}", keys.raw),
        ] {
            let text = format!("{}{}{}", "[".repeat(depth), leaf, "]".repeat(depth));
            base["descriptor"]["pipeline"] = json!({keys.raw: text});
            compare(&serde_json::to_string(&base).unwrap());
        }
    }
}
