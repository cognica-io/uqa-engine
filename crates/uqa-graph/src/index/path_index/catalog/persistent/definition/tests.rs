//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{decode_image, ImageDecodeWindow, ImageDecoder};
use uqa_storage::read_control::StorageReadControl;

#[test]
fn stored_path_sequences_preserve_compact_serde_spelling_and_diagnostics() {
    let value = format!(
        r#"[[],["{}\n\u0001\uD83D\uDE03\/\\\""],["duplicate","duplicate"]]"#,
        "가".repeat(256)
    );
    let control = StorageReadControl::with_limit(1 << 20);
    for text in [
        value.as_str(),
        " [] ",
        "[[],[]]",
        r#"[["one"],["one"]]"#,
        "",
        " ",
        "[",
        "[[",
        "[1]",
        "[[]] true",
        "{}",
        "[{}]",
        "[[{}]]",
        "[[true]]",
        "[[false]]",
        "[[null]]",
        "[[12]]",
        "[[  []]]",
        "[[],]",
        r#"[["a",]]"#,
        r#"[["\ud800"]]"#,
        r#"[[false, "after"]]"#,
    ] {
        let ordinary = serde_json::from_str::<Vec<Vec<String>>>(text);
        let result = decode_image(
            &mut std::io::empty(),
            control.memory(),
            control.cancellation(),
            async |mut input| read(text, &mut input).await,
        );
        match (ordinary, result) {
            (Ok(sequences), Ok(result)) => {
                assert_eq!(result.sequences, sequences);
                assert_eq!(result.json, serde_json::to_string(&sequences).unwrap());
            }
            (Err(expected), Err(actual)) => assert_eq!(
                actual.to_string(),
                StorageBackendError::Serde(expected).to_string(),
                "{text:?}"
            ),
            _ => panic!("definition acceptance differed for {text:?}"),
        }
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn a_single_wide_path_definition_yields_for_sequences_labels_and_utf8_fragments() {
    let inner = std::iter::repeat_n(r#""x""#, 4096)
        .collect::<Vec<_>>()
        .join(",");
    let outer = std::iter::repeat_n("[]", 4096)
        .collect::<Vec<_>>()
        .join(",");
    for text in [
        format!("[[{inner},false]]"),
        format!("[{outer},false]"),
        format!("[[\"{}\",false]]", "가\\n".repeat(32768)),
    ] {
        let mut polls = Vec::new();
        for (steps, cancel) in [(1, false), (32, false), (1, true)] {
            let control = StorageReadControl::with_limit(8 << 20);
            let stored = text.clone();
            let mut input = ImageDecoder::new(
                control.memory(),
                control.cancellation(),
                |mut channel| async move { read(&stored, &mut channel.input()).await },
            )
            .unwrap();
            let mut count = 0;
            let error = loop {
                count += 1;
                if cancel && count == 101 {
                    control.cancellation().cancel();
                }
                match input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps }) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("late type mismatch must reject"),
                    Err(error) => break error,
                }
            };
            if cancel {
                assert!(matches!(error, StorageBackendError::Cancelled(_)));
                assert_eq!(count, 101);
            } else {
                assert_eq!(
                    error.to_string(),
                    StorageBackendError::Serde(
                        serde_json::from_str::<Vec<Vec<String>>>(&text).unwrap_err()
                    )
                    .to_string()
                );
                polls.push(count);
            }
            drop((error, input));
            assert_eq!(control.memory().used(), 0);
        }
        assert!(polls[0] > 1000 && polls[1] < polls[0] / 16, "{polls:?}");
    }
}
