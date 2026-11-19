//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::GraphLabelRegistry;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    CancellationToken,
};

fn decoder(
    text: Option<&str>,
    control: &StorageReadControl,
    invoking: &CancellationToken,
) -> ImageDecoder<NativeLabels, StorageBackendError> {
    let source = text.map(|text| {
        ProductionControl::new(control.memory(), control.cancellation(), invoking)
            .copy_text(text)
            .unwrap()
            .into_budgeted()
            .unwrap()
    });
    let retained = control.clone();
    ImageDecoder::new(control.memory(), invoking, |mut channel| async move {
        NativeLabels::read_stored_from(
            source.as_deref().map(String::as_str),
            &retained,
            &mut channel.input(),
        )
        .await
    })
    .unwrap()
}

fn decoded(text: Option<&str>) -> Result<GraphLabelRegistry, String> {
    let control = StorageReadControl::with_limit(4 << 20);
    let result = decoder(text, &control, control.cancellation()).read(&mut std::io::empty());
    let result = result
        .map(|labels| labels.to_record())
        .map_err(|error| error.to_string());
    assert_eq!(control.memory().used(), 0);
    result
}

fn compare(text: &str) {
    let expected = if text.is_empty() {
        Ok(GraphLabelRegistry::default())
    } else {
        serde_json::from_str::<GraphLabelRegistry>(text)
    };
    let actual = decoded(Some(text));
    assert_eq!(
        actual.is_ok(),
        expected.is_ok(),
        "{text:?}: {actual:?}; {expected:?}"
    );
    if let Ok(expected) = expected {
        assert_eq!(actual.unwrap(), expected, "{text}");
    }
}

#[test]
fn stored_graph_labels_match_original_defaults_fields_and_positional_input() {
    assert_eq!(decoded(None).unwrap(), GraphLabelRegistry::default());
    for text in [
        "",
        "{}",
        "[]",
        "null",
        "false",
        "0",
        "\"registry\"",
        " ",
        "{} {}",
        r#"{"labels":{"b":7,"a":5,"b":9},"kinds":{"a":"v","b":{"e":null}},"sequences":{"7":11,"7":12},"dropped_label_ids":[1,7,7],"next_label_id":0}"#,
        r#"{"unknown":{"nested":["\ud800",1e999999999999999999999]}}"#,
        r#"{"labels":{},"labels":{}}"#,
        r#"{"labels":{},"labe\u006cs":{}}"#,
        r#"{"unknown":1,"unknown":2}"#,
        r#"{"unknown\ud800":0}"#,
        r#"{"labels":{"\ud800":4}}"#,
        r#"{"kinds":{"node":{"v":null,"e":null}}}"#,
        r#"{"kinds":{"node":"vertex"}}"#,
    ] {
        compare(text);
    }
    let fields = [
        "[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15]",
        r#"{"node":8}"#,
        r#"{"node":"e"}"#,
        r#"{"8":99}"#,
        "[1,3]",
        "4294967295",
    ];
    for count in 0..=fields.len() {
        compare(&format!("[{}]", fields[..count].join(",")));
    }
    compare(&format!("[{},0]", fields.join(",")));
    for field in [
        "allocation_id",
        "labels",
        "kinds",
        "sequences",
        "dropped_label_ids",
        "next_label_id",
    ] {
        compare(&format!(r#"{{"{field}":null}}"#));
    }
    compare(&format!(
        r#"{{"ignored":{}0{}}}"#,
        "[".repeat(512),
        "]".repeat(512)
    ));
}

#[test]
fn stored_graph_labels_preserve_integer_and_numeric_map_key_acceptance() {
    for number in [
        "0",
        "-0",
        "1",
        "255",
        "256",
        "4294967295",
        "4294967296",
        "18446744073709551615",
        "18446744073709551616",
        "-1",
        "1.0",
        "1e0",
        "01",
        "+1",
        "1e9999",
        "1 ",
        " 1",
        "0.0",
    ] {
        for text in [
            format!(r#"{{"next_label_id":{number}}}"#),
            format!(r#"{{"labels":{{"node":{number}}}}}"#),
            format!(r#"{{"sequences":{{"3":{number}}}}}"#),
            format!(r#"{{"sequences":{{"{number}":0}}}}"#),
            format!(r#"{{"dropped_label_ids":[{number}]}}"#),
            format!(r#"{{"allocation_id":[{number},0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}}"#),
        ] {
            compare(&text);
        }
    }
    for key in [r"\u0031", "", "1.5", "1e1", "01", "-0"] {
        compare(&format!(r#"{{"sequences":{{"{key}":1}}}}"#));
    }
}

fn wide() -> String {
    serde_json::to_string(&GraphLabelRegistry {
        labels: (0..128)
            .map(|id| (format!("{}-{id}", "label\0한글".repeat(20)), id + 3))
            .collect(),
        kinds: [("node\0한글".repeat(2048), LabelKind::Edge)].into(),
        sequences: (0..128).map(|id| (id + 3, u64::from(id) + 10)).collect(),
        dropped_label_ids: (0..128).collect(),
        ..GraphLabelRegistry::default()
    })
    .unwrap()
}

#[test]
fn stored_graph_label_continuation_retains_native_roots_and_original_allowance() {
    let text = wide();
    let expected: GraphLabelRegistry = serde_json::from_str(&text).unwrap();
    let control = StorageReadControl::with_limit(8 << 20);
    let mut input = decoder(Some(&text), &control, control.cancellation());
    let mut polls = 0;
    let result = loop {
        polls += 1;
        assert!(polls < 100_000);
        if let Some(value) = input
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            )
            .unwrap()
        {
            break value;
        }
    };
    assert!(polls > 512);
    assert_eq!(result.to_record(), expected);
    let clone = result.clone();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    assert_eq!(clone.to_record(), expected);
    drop((result, clone, input));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn stored_graph_label_input_retires_partial_roots_on_every_terminal_path() {
    let text = wide();
    for terminal in ["original", "invoking", "zero", "memory", "drop", "unwind"] {
        let control = StorageReadControl::with_limit(8 << 20);
        let invoking = CancellationToken::new();
        let mut input = decoder(Some(&text), &control, &invoking);
        for _ in 0..90 {
            assert!(input
                .advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 }
                )
                .unwrap()
                .is_none());
        }
        let held = if terminal == "memory" {
            Some(
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap(),
            )
        } else {
            None
        };
        match terminal {
            "original" => control.cancellation().cancel(),
            "invoking" => invoking.cancel(),
            "unwind" => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _input = input;
                    panic!("retire stored label input");
                }));
                assert_eq!(control.memory().used(), 0);
                continue;
            }
            _ => (),
        }
        if terminal != "drop" {
            let steps = usize::from(terminal != "zero");
            loop {
                match input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps }) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("terminal label input completed"),
                    Err(_) => break,
                }
            }
            assert!(input
                .advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 }
                )
                .is_err());
        }
        drop((input, held));
        assert_eq!(control.memory().used(), 0);
    }
}
