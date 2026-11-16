//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::json::JsonReader;
use std::collections::BTreeMap;

fn compare_map(text: &str) {
    let expected = serde_json::from_str::<BTreeMap<String, serde_json::Value>>(text);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut reader =
        JsonReader::from_slice_with_control(text.as_bytes(), &control).with_depth_limit(127);
    let actual = crate::json::work::inline(async {
        let document =
            JsonDocument::read_map_with_work(&mut reader, &control, &mut (), protocol()).await?;
        assert!(reader
            .next_event_with_work(&mut ())
            .await
            .map_err(|error| JsonInputError::from_reader(&reader, error))?
            .is_none());
        document.root().to_json_with_control(&control)
    });
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => assert_eq!(
            &*actual,
            &serde_json::to_string(&expected).unwrap(),
            "{text}"
        ),
        (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{text}");
            assert_eq!(actual.is_data(), expected.is_data());
            assert_eq!(actual.is_eof(), expected.is_eof());
        }
        other => panic!("typed map differs: {other:?}"),
    }
    drop(reader);
    assert_eq!(memory.used(), 0);
}

#[test]
fn typed_generic_maps_preserve_literal_keys_nested_protocols_and_replacement_order() {
    for text in [
        "{}",
        "[]",
        "null",
        "1",
        "{",
        r#"{"a":1,"a":2}"#,
        r#"{"a":true,"a":"\ud800"}"#,
    ] {
        compare_map(text);
    }
    for key in [protocol().number, protocol().raw] {
        for value in ["1", "true", "[]", "\"1.25\"", "\"bad\""] {
            compare_map(&format!("{{{key:?}:{value}}}"));
            compare_map(&format!("{{\"value\":{{{key:?}:{value}}}}}"));
            compare_map(&format!("{{\"value\":{{{key:?}:{value}}},\"value\":0}}"));
        }
    }
}

#[test]
fn typed_generic_map_continuation_preserves_parent_position_and_admitted_retirement() {
    use crate::image_io::{ImageDecodeWindow, ImageDecoder};
    let text = format!("[{{\"value\":[{}]}},42]", "0,".repeat(2048) + "1");
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let text = Budgeted::new(text.clone(), memory.reserve(text.len()).unwrap());
    let mut input: ImageDecoder<(), JsonInputError> =
        ImageDecoder::new(&memory, &token, |mut channel| async move {
            let mut work = channel.input();
            let control = ProductionControl::new(work.memory, work.cancellation, work.cancellation);
            let mut reader = JsonReader::from_slice_with_control(text.as_bytes(), &control);
            assert!(matches!(
                reader.next_event_from(&mut work).await?.unwrap().token,
                crate::json::JsonToken::StartArray
            ));
            assert!(reader.next_array_value_with_work(&mut work).await?);
            let map =
                JsonDocument::read_map_with_work(&mut reader, &control, &mut work, protocol())
                    .await?;
            assert_eq!(
                map.root().get("value").unwrap().elements().unwrap().count(),
                2049
            );
            map.retire_with_work(&control, &mut work).await?;
            assert!(reader.next_array_value_with_work(&mut work).await?);
            assert_eq!(reader.read_u32_from(&mut work).await?, 42);
            reader.finish_array_with_work(&mut work).await?;
            assert!(reader.next_event_from(&mut work).await?.is_none());
            Ok(())
        })
        .unwrap();
    let mut polls = 0;
    while input
        .advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 },
        )
        .unwrap()
        .is_none()
    {
        polls += 1;
        assert!(polls < 100_000);
    }
    assert!(polls > 4096);
    drop(input);
    assert_eq!(memory.used(), 0);
}
