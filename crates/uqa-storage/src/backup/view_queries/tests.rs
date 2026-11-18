//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn project(
    source: &[u8],
    envelope: Envelope,
    window: usize,
    maximum: usize,
) -> BackupResult<Vec<u8>> {
    let control = StorageReadControl::with_limit(8192);
    let mut row = Row::new(0..source.len(), envelope, false, &control);
    loop {
        let start = row.offset;
        row.offset = (start + window).min(source.len());
        match row.feed(&source[start..row.offset], &control, maximum)? {
            Next::More => (),
            Next::Legacy => row = Row::new(0..source.len(), envelope, true, &control),
            Next::Done => return Ok(row.output.to_vec()),
        }
    }
}

const TEXT: Envelope = Envelope::Text(|_| unreachable!());

#[test]
fn query_projection_preserves_original_bytes_across_escapes_and_previous_nested_fields() {
    let query = r#"{ "ctes":[], "root":{"literal":"],}\"🌱\t\\"} }"#;
    let ignored =
        serde_json::to_string(&vec!["fake \"query\": [{}]", &"x".repeat(64 << 10)]).unwrap();
    let definition = format!(
        r#"{{"materialized_rows":{ignored},"arbitrary":null,"x":7,"qu\u0065ry": {query} }}"#
    );
    let encoded = serde_json::to_string(&definition)
        .unwrap()
        .replace('🌱', r"\ud83c\udf31");
    let wrapper = format!(r#"{{"before":{ignored},"definition\u005fjson":{encoded}}}"#);
    for window in [1, 5, 127, 1024] {
        assert_eq!(
            project(definition.as_bytes(), TEXT, window, 128).unwrap(),
            query.as_bytes()
        );
        assert_eq!(
            project(wrapper.as_bytes(), Envelope::KeyValue, window, 128).unwrap(),
            query.as_bytes()
        );
    }
}

#[test]
fn legacy_queries_keep_their_representation_and_missing_fields_retire_the_input() {
    let current = r#"[ [0,0], { "from":"current" }, [{"text":"irrelevant"}] ]"#;
    let legacy = r#"{ "root" : {"braces":"{}"}, "ctes":[] }"#;
    let positional = r#"[false,[],{"node":2}]"#;
    for (source, expected) in [
        (current, r#"{ "from":"current" }"#),
        (legacy, legacy),
        (positional, positional),
    ] {
        let wrapper = serde_json::json!({"definition_json": source}).to_string();
        assert_eq!(
            project(source.as_bytes(), TEXT, 2, 128).unwrap(),
            expected.as_bytes()
        );
        assert_eq!(
            project(wrapper.as_bytes(), Envelope::KeyValue, 2, 128).unwrap(),
            expected.as_bytes()
        );
    }
    for missing in [
        b"".as_slice(),
        b"{}",
        br#"{"materialized_rows":["query"]}"#,
        br#"{"query":null}"#,
    ] {
        assert!(matches!(
            project(missing, TEXT, 1, 80),
            Err(BackupError::Invalid(_))
        ));
    }
    assert!(matches!(
        project(
            br#"{"unknown":"definition_json"}"#,
            Envelope::KeyValue,
            1,
            80
        ),
        Err(BackupError::Invalid(_))
    ));
    assert!(matches!(
        project(legacy.as_bytes(), TEXT, 1, 7),
        Err(BackupError::Memory(_))
    ));
}
