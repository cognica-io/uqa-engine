//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_binding_typed_envelopes_preserve_field_sequence_and_enum_diagnostics() {
    for source in [
        "{} trailing",
        "[]",
        "null",
        "true",
        "0",
        "\"text\"",
        "[\"x\"]",
        "[\"x\",1]",
        "[\"x\",1,\"field\"]",
        "[\"x\",1,\"field\",\"both\"]",
        "[\"x\",1,\"field\",\"both\",[null,null]]",
        "[\"x\",1,\"field\",\"both\",[null,null],[null,null],false]",
        "[\"x\",1,\"field\",\"both\",[null,null],[null,null],",
        "[\"x\",1,\"field\",\"both\",[null,null],[null,null],]",
        "{\"unknown\":",
        "{\"한🙂\":",
        "{\"index\":{}}",
        "{\"index\":[]}",
        "{\"index\":[null]}",
        "{\"index\":[null,null,false]}",
        "{\"index\":{\"unknown\":",
        "{\"format\":\"x\",\"format\":",
        "{\"index\":{\"name\":null,\"name\":",
        "{\"index\":{\"descriptor\":null,\"descriptor\":",
    ] {
        compare_diagnostic(source);
    }
    for field in ["owner", "last_phase"] {
        let known = if field == "owner" { "field" } else { "both" };
        for value in [
            "{}".into(),
            "[]".into(),
            "null".into(),
            "true".into(),
            "0".into(),
            "\"unknown\"".into(),
            "{\"unknown\":malformed}".into(),
            "{null:0}".into(),
            "{0:0}".into(),
            "{1.5:0}".into(),
            "{[]:0}".into(),
            format!("{{\"{known}\":false}}"),
            format!("{{\"{known}\":[}}"),
            format!("{{\"{known}\":{{}}}}"),
            format!("{{\"{known}\":1e999999}}"),
            format!("{{\"{known}\":null,}}"),
            format!("{{\"{known}\":null,\"extra\":null}}"),
            format!("{{\"{known}\":null"),
        ] {
            let source = format!("{{\n \"{field}\":{value}}}");
            for end in (0..=source.len()).filter(|&end| source.is_char_boundary(end)) {
                compare_diagnostic(&source[..end]);
            }
        }
    }
    for value in [
        "-0",
        "4294967296",
        "-1",
        "18446744073709551616",
        "1.5",
        "1e400",
        "1e99999999999999999999",
        "-\n",
        "1.\n",
        "1e+\n",
        "01",
    ] {
        for source in [
            format!("{{\n \"version\": {value}, \"bad\":"),
            format!("[\"x\",\n {value}, malformed]"),
        ] {
            compare_diagnostic(&source);
        }
    }
    let source = r#"{"format":"x","version":1,"owner":{"field":null},"last_phase":{"both":null},"index":[null,null],"search":[null,null]}"#;
    for offset in 0..source.len() {
        for byte in [0, b'"', b',', b':', b']', b'}', b'\n'] {
            let mut changed = source.as_bytes().to_vec();
            changed[offset] = byte;
            let changed = std::str::from_utf8(&changed).unwrap();
            if serde_json::from_str::<super::super::super::StoredBinding>(changed).is_err() {
                compare_diagnostic(changed);
            }
        }
    }
    let mut value: serde_json::Value = serde_json::from_str(&binding().to_json().unwrap()).unwrap();
    value["owner"] = serde_json::json!({"field":null});
    value["last_phase"] = serde_json::json!({"search":null});
    compare(&value.to_string());
}

#[test]
fn stored_binding_unknown_names_retain_original_ownership_until_error_redaction() {
    let secret = "private한🙂".repeat(8192);
    for source in [
        format!("{{\"{secret}\":malformed"),
        format!("{{\"owner\":\"{secret}\"}}"),
        format!("{{\"index\":{{\"{secret}\":malformed"),
    ] {
        let memory = MemoryBudget::new(8 << 20);
        let mut read = decoder(&source, &memory, &CancellationToken::new());
        let StorageBackendError::Json(error) = complete(&mut read, 1).unwrap_err() else {
            panic!("missing native diagnostic")
        };
        drop(read);
        assert!(memory.used() >= secret.len());
        assert!(error.to_string().contains(&secret));
        let position = error.position();
        let redacted = error.into_type_only();
        assert_eq!(memory.used(), 0);
        assert_eq!(redacted.position(), position);
        assert!(redacted.is_data());
        assert!(!redacted.to_string().contains("private"));
        assert!(!format!("{redacted:?}").contains("private"));
    }
}
