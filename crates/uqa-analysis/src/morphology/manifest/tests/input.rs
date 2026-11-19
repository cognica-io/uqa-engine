//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    canonical_fixture, complete, Arc, CancellationToken, ImageDecoder, MemoryBudget, Value,
};
use crate::morphology::{manifest, DictionaryError};

#[test]
fn native_provenance_json_constructs_original_values_through_retained_token_windows() {
    let (_, large) = canonical_fixture();
    let samples = [
        large,
        br#" {"string":"\uD842\uDFB7\u0000\"\\\/\b\f\n\r\t", "array":[true,false,null,1.0,-0.0,1E+40],"dup":1,"dup":2} "#.to_vec(),
        b"null".to_vec(),
        b"-0".to_vec(),
        b"1234567890123456789012345678901234567890".to_vec(),
    ];
    for bytes in samples {
        let expected: Value = serde_json::from_slice(&bytes).unwrap();
        let bytes = Arc::new(bytes);
        for steps in [1, 17, 512] {
            let memory = MemoryBudget::new(1 << 20);
            let cancellation = CancellationToken::new();
            let source = bytes.clone();
            let mut input = ImageDecoder::<_, DictionaryError>::new(
                &memory,
                &cancellation,
                |mut channel| async move { manifest::parse(&source, &mut channel.input()).await },
            )
            .unwrap();
            let (actual, _) = complete(&mut input, steps);
            assert_eq!(actual, expected);
            assert_eq!(memory.used(), 0);
            assert_eq!(
                serde_json::to_vec(&actual).unwrap(),
                serde_json::to_vec(&expected).unwrap()
            );
        }
    }
}

#[test]
fn native_provenance_json_preserves_reference_syntax_and_nesting_limits() {
    let mut samples = vec![
        b"".to_vec(),
        b"{".to_vec(),
        b"[1,".to_vec(),
        b"1 2".to_vec(),
        b"01".to_vec(),
        b"+1".to_vec(),
        b"1.".to_vec(),
        b"1e+".to_vec(),
        b"[1,]".to_vec(),
        b"{\"a\":1,}".to_vec(),
        b"{1:2}".to_vec(),
        br#""\uD800""#.to_vec(),
        br#""\uDC00""#.to_vec(),
        br#""\z""#.to_vec(),
        vec![b'"', 0xff, b'"'],
        vec![b'"', 0, b'"'],
    ];
    for depth in [0, 1, 126, 127, 128, 129] {
        samples.push(format!("{}null{}", "[".repeat(depth), "]".repeat(depth)).into_bytes());
        samples.push(format!("{}null{}", "{\"a\":".repeat(depth), "}".repeat(depth)).into_bytes());
    }
    let text = br#"{"data":[null,true,false,-42.50e+2,"\uD842\uDFB7"]}"#;
    samples.extend((0..=text.len()).map(|end| text[..end].to_vec()));
    for bytes in samples {
        let expected = serde_json::from_slice::<Value>(&bytes);
        let actual = crate::work::inline(manifest::parse(&bytes, &mut ()));
        assert_eq!(actual.is_ok(), expected.is_ok(), "{bytes:?}");
        if let (Ok(actual), Ok(expected)) = (actual, expected) {
            assert_eq!(actual, expected, "{bytes:?}");
        }
    }
}
