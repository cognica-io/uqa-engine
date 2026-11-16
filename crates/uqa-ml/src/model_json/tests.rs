//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

mod resources;

fn decoder(
    text: &str,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> ImageDecoder<Budgeted<DeepModel>, JsonInputError> {
    let reservation = memory.reserve(text.len()).unwrap();
    let text = Budgeted::new(text.to_owned(), reservation);
    ImageDecoder::new(memory, cancellation, |mut channel| async move {
        read_model_json_from(&text, &mut channel.input()).await
    })
    .unwrap()
}

fn finish(
    input: &mut ImageDecoder<Budgeted<DeepModel>, JsonInputError>,
    steps: usize,
) -> Result<Budgeted<DeepModel>> {
    for _ in 0..2_000_000 {
        if let Some(model) =
            input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
        {
            return Ok(model);
        }
    }
    panic!("model input stopped progressing");
}

fn compare(text: &str) {
    let expected = serde_json::from_str::<DeepModel>(text);
    for steps in [1, 37] {
        let memory = MemoryBudget::new(16 << 20);
        let mut input = decoder(text, &memory, &CancellationToken::new());
        let actual = finish(&mut input, steps);
        match (&expected, actual) {
            (Ok(expected), Ok(actual)) => {
                assert_eq!(&*actual, expected, "{text}");
                assert_eq!(actual.alpha.to_bits(), expected.alpha.to_bits(), "{text}");
                assert_eq!(
                    serde_json::to_string(&*actual).unwrap(),
                    serde_json::to_string(expected).unwrap(),
                    "{text}"
                );
                drop(actual);
            }
            (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
                assert_eq!(actual.to_string(), expected.to_string(), "{text:?}");
                assert_eq!(actual.is_data(), expected.is_data(), "{text:?}");
                assert_eq!(actual.is_syntax(), expected.is_syntax(), "{text:?}");
                assert_eq!(actual.is_eof(), expected.is_eof(), "{text:?}");
                assert_eq!(actual.position().line, expected.line());
                assert_eq!(actual.position().column, expected.column());
            }
            (expected, actual) => panic!("{text:?}: {expected:?} / {actual:?}"),
        }
        drop(input);
        assert_eq!(memory.used(), 0);
    }
}

const LAYERS: &[&str] = &[
    r#"{"kind":"input","dimensions":7}"#,
    r#"{"kind":"embed","embedding":[-0.0,0,0.1,1.2345678901234567,1e400]}"#,
    r#"{"kind":"dense","weights":[1,2],"bias":[-0.0],"output_channels":1,"input_channels":2}"#,
    r#"{"kind":"flatten"}"#,
    r#"{"kind":"global_pool","method":"avg_max"}"#,
    r#"{"kind":"softmax"}"#,
    r#"{"kind":"batch_norm","epsilon":1e-5}"#,
    r#"{"kind":"dropout","p":0.25}"#,
    r#"{"kind":"cnn_1d","weights":[0.1],"bias":[],"output_channels":1,"input_channels":1,"kernel_size":1,"stride":1,"padding":0}"#,
    r#"{"kind":"cnn_2d","weights":[0.2],"bias":[],"output_channels":1,"input_channels":1,"input_height":2,"input_width":3,"kernel_height":1,"kernel_width":1,"stride_height":1,"stride_width":1,"padding_height":0,"padding_width":0}"#,
    r#"{"kind":"propagate","edge_label":"관계🙂","aggregation":{"sum":null},"direction":"both"}"#,
    r#"{"kind":"conv","edge_label":"edge","hop_weights":[1,-0.0],"direction":{"in":null}}"#,
    r#"{"kind":"pool","edge_label":"edge","pool_size":3,"method":"max","direction":"out"}"#,
    r#"{"kind":"attention"}"#,
    r#"{"kind":"rnn","weights_input":[1],"weights_hidden":[2],"bias":[3],"hidden_channels":1,"input_channels":1,"return_sequences":true}"#,
    r#"{"kind":"lstm","weights_input":[],"weights_hidden":[],"bias":[],"hidden_channels":0,"input_channels":0,"return_sequences":false}"#,
];

#[test]
fn stored_model_json_preserves_all_layer_fields_and_original_numeric_boundaries() {
    for layer in LAYERS {
        compare(&format!(
            r#"{{"layers":[{layer}],"alpha":-0.0,"gating":"re_l_u"}}"#
        ));
        compare(&format!("[[{layer}]]"));
    }
    compare(&format!(r#"{{"layers":[{}]}}"#, LAYERS.join(",")));
    for number in [
        "-0",
        "-0.0",
        "0.1",
        "1.2345678901234567",
        "1e-400",
        "1e400",
        "18446744073709551616",
    ] {
        compare(&format!(
            r#"{{"layers":[{{"kind":"embed","embedding":[{number}]}}],"alpha":{number}}}"#
        ));
    }
}

#[test]
fn stored_model_json_preserves_defaults_duplicates_unknowns_and_first_failures() {
    for text in [
        "",
        "null",
        "true",
        "3",
        "-0",
        "1.5",
        "1e400",
        "\"text\"",
        "{}",
        "[]",
        "[[]]",
        "[[],0]",
        "[[],0,\"none\"]",
        "[[],0,\"none\",false]",
        r#"{"layers":[],"layers":null}"#,
        r#"{"alpha":false,"layers":null}"#,
        r#"{"layers":[],"alpha":1,"alpha":false}"#,
        r#"{"layers":[],"gating":"none","gating":0}"#,
        r#"{"layers":[],"unknown":"\ud800"}"#,
        r#"{"layers":[],"unknown":[{"anything":1e400}]}"#,
        r#"{"layers":{},"alpha":false}"#,
        r#"{"layers":[null]}"#,
        r#"{"layers":[{}]}"#,
        r#"{"layers":[{"kind":true}]}"#,
        r#"{"layers":[{"kind":"bad"}]}"#,
        r#"{"layers":[{"kind":"input"}]}"#,
        r#"{"layers":[{"kind":"dense","weights":[true]}]}"#,
        r#"{"layers":[{"kind":"input","dimensions":false,"ignored":"\ud800"}]}"#,
        r#"{"layers":[{"kind":"input","dimensions":1,"dimensions":2}]}"#,
        r#"{"layers":[{"kind":"bad","kind":"flatten"}]}"#,
        r#"{"layers":[{"kind":"conv","edge_label":false}]}"#,
        r#"{"layers":[{"kind":"global_pool","method":{"avg":0}}]}"#,
        r#"{"layers":[{"kind":"global_pool","method":{"bad":false,"max":null}}]}"#,
        r#"{"layers":[{"kind":"global_pool","method":{"bad":false}}]}"#,
        r#"{"layers":[{"kind":"global_pool","method":{}}]}"#,
        r#"{"layers":[{"kind":"global_pool","method":false}]}"#,
        r#"{"layers":[{"kind":"bad"}, {},"#,
        "{\"layers\":[{\"kind\":\"bad\"}  , \n  {]}",
        r#"{"layers":[],"gating":{"swish":null}}"#,
        r#"{"layers":[]} false"#,
    ] {
        compare(text);
    }
    for number in [
        "-1",
        "1.0",
        "1e0",
        "18446744073709551616",
        "null",
        "[]",
        "\"bad\"",
    ] {
        compare(&format!(
            r#"{{"layers":[{{"kind":"input","dimensions":{number}}}]}}"#
        ));
    }
}

#[test]
fn stored_model_json_preserves_truncation_and_nested_generic_protocols() {
    let sample = r#"{"layers":[{"kind":"input","dimensions":3}],"alpha":0.25,"gating":"none"}"#;
    for end in 0..sample.len() {
        compare(&sample[..end]);
    }
    let protocol = uqa_core::json::JsonScalarProtocol::from_values(
        &serde_json::Number::from(1_u8),
        serde_json::value::RawValue::NULL,
    )
    .unwrap();
    for key in [protocol.number, protocol.raw] {
        for text in ["1.25", "1e400", "false", "01", "[1,]", "\"flatten\""] {
            let value = serde_json::to_string(text).unwrap();
            compare(&format!(
                r#"{{"layers":[{{"kind":"dropout","p":{{{key:?}:{value}}}}}]}}"#
            ));
            compare(&format!(
                r#"{{"layers":[{{{key:?}:{value},"kind":"flatten"}}]}}"#
            ));
            compare(&format!(
                r#"{{"layers":[{{"kind":"flatten","ignored":{{{key:?}:{value}}}}}]}}"#
            ));
        }
    }
    for depth in [124, 125, 126, 127, 128, 129] {
        let value = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        compare(&format!(r#"{{"layers":[],"ignored":{value}}}"#));
        compare(&format!(
            r#"{{"layers":[{{"kind":"flatten","ignored":{value}}}]}}"#
        ));
    }
}
