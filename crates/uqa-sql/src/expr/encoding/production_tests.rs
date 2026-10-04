//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn hashes_preserve_vectors_and_both_final_block_lengths_without_input_copies() {
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    // MD5's only heap allocation is its 32-byte returned text, even for many input blocks.
    let budget = MemoryBudget::new(32);
    let control = ProductionControl::new(&budget, &original, &invoking);
    for (input, expected) in [
        (Vec::new(), "d41d8cd98f00b204e9800998ecf8427e"),
        (b"abc".to_vec(), "900150983cd24fb0d6963f7d28e17f72"),
        (vec![b'a'; 55], "ef1772b6dff9a122358552954ad0df65"),
        (vec![b'a'; 56], "3b0c8ac703f828b04c6c197006d17218"),
        (vec![b'a'; 63], "b06521f39153d618550606be297466d5"),
        (vec![b'a'; 64], "014842d480b571495a4a0363793f7367"),
        (vec![b'a'; 65], "c743a45e0d2e6a95cb859adae0248435"),
        (vec![b'a'; 8192], "221994040b14294bdf7fbc128e66633c"),
    ] {
        let output = md5_hex_with_control(&input, &control).unwrap();
        assert_eq!(&*output, expected);
        assert_eq!(md5_hex(&input), expected);
        assert_eq!(budget.used(), 32);
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn base64_and_hex_preserve_padding_binary_and_existing_error_behavior() {
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let budget = MemoryBudget::new(4096);
    let control = ProductionControl::new(&budget, &original, &invoking);
    for (bytes, encoded) in [
        (b"".as_slice(), ""),
        (b"f", "Zg=="),
        (b"fo", "Zm8="),
        (b"foo", "Zm9v"),
        (b"foobar", "Zm9vYmFy"),
        (&[0, 255, 128], "AP+A"),
    ] {
        let output = base64_encode_with_control(bytes, &control).unwrap();
        assert_eq!(&*output, encoded);
        assert_eq!(base64_encode(bytes), encoded);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        let output = base64_decode_with_control(encoded, &control).unwrap();
        assert_eq!(&**output, bytes);
        assert_eq!(base64_decode(encoded).unwrap(), bytes);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    let output = hex_encode_with_control(&[0, 1, 127, 128, 255], &control).unwrap();
    assert_eq!(&*output, "00017f80ff");
    drop(output);
    for input in ["Z", "Zg=", "====", "Zg===", "!", "Zg== "] {
        let ordinary = base64_decode(input);
        let controlled = base64_decode_with_control(input, &control);
        match (ordinary, controlled) {
            (Ok(expected), Ok(output)) => assert_eq!(&*output, &expected),
            (Err(expected), Err(error)) => assert_eq!(error.to_string(), expected.to_string()),
            _ => panic!("base64 modes diverged for {input:?}"),
        }
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn encoding_quota_and_both_cancellation_scopes_release_every_buffer() {
    for limit in [0, 1, 2, 8, 31] {
        let budget = MemoryBudget::new(limit);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        for error in [
            md5_hex_with_control(&[42; 8192], &control).unwrap_err(),
            base64_encode_with_control(&[42; 512], &control).unwrap_err(),
            base64_decode_with_control(&"YQ==".repeat(64), &control).unwrap_err(),
            hex_encode_with_control(&[42; 64], &control).unwrap_err(),
        ] {
            assert_eq!(error.sqlstate(), Some("53200"));
            assert_eq!(budget.used(), 0);
        }
    }
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        for error in [
            md5_hex_with_control(b"abc", &control).unwrap_err(),
            base64_encode_with_control(b"abc", &control).unwrap_err(),
            base64_decode_with_control("YWJj", &control).unwrap_err(),
            hex_encode_with_control(b"abc", &control).unwrap_err(),
        ] {
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn bytea_text_formats_follow_postgresql_encode_c() {
    let control = ProductionControl::uncontrolled();
    let encode_base64 = |input: &[u8]| {
        base64_encode_with_control(input, &control)
            .unwrap()
            .into_uncontrolled()
            .unwrap()
    };
    // `pg_base64_encode` breaks lines after 76 characters, even when the output ends there.
    let full_line = encode_base64(&[b'a'; 57]);
    assert_eq!((full_line.len(), full_line.ends_with('\n')), (77, true));
    let next_line = encode_base64(&[b'a'; 58]);
    assert_eq!((next_line.len(), next_line.find('\n')), (81, Some(76)));
    assert_eq!(base64_decode(&next_line).unwrap(), vec![b'a'; 58]);

    // Whitespace is skipped, and padding that ends a sequence limits every later group.
    assert_eq!(base64_decode(" Zm9v\nYmFy ").unwrap(), b"foobar");
    assert_eq!(base64_decode("Zg==Zm9v").unwrap(), b"ff");
    assert_eq!(base64_decode("Zg=x").unwrap(), b"f");
    for (input, message, hint) in [
        (
            "A?==",
            "invalid symbol \"?\" found while decoding base64 sequence",
            None,
        ),
        (
            "\u{e9}",
            "invalid symbol \"\u{e9}\" found while decoding base64 sequence",
            None,
        ),
        ("=", "unexpected \"=\" while decoding base64 sequence", None),
        (
            "AQ",
            "invalid base64 end sequence",
            Some("Input data is missing padding, is truncated, or is otherwise corrupted."),
        ),
    ] {
        let error = base64_decode(input).unwrap_err();
        let error_hint = match &error {
            SQLError::Diagnostic { hint, .. } => hint.clone(),
            _ => None,
        };
        assert_eq!(
            (
                error.sqlstate(),
                error.to_string().as_str(),
                error_hint.as_deref()
            ),
            (Some("22023"), message, hint),
            "{input:?}"
        );
    }

    let hex = |input: &str| {
        hex_decode_with_control(input, &control)
            .map(|bytes| bytes.into_uncontrolled().unwrap())
            .map_err(|error| error.to_string())
    };
    assert_eq!(hex("01 02\t0A\r\n"), Ok(vec![1, 2, 10]));
    assert_eq!(hex("0 1"), Err("invalid hexadecimal digit: \" \"".into()));
    // Only spaces, tabs and line breaks separate digit pairs; other Unicode whitespace is a digit error.
    assert_eq!(
        hex("61\u{2003}62"),
        Err("invalid hexadecimal digit: \"\u{2003}\"".into())
    );
    assert_eq!(
        hex("012"),
        Err("invalid hexadecimal data: odd number of digits".into())
    );

    // `esc_enc` escapes only NUL, high-bit bytes and the backslash.
    let escaped = escape_encode_with_control(&[0x5c, 0, 1, 0xff, 0x7f, b'A'], &control)
        .unwrap()
        .into_uncontrolled()
        .unwrap();
    assert_eq!(escaped, "\\\\\\000\u{1}\\377\u{7f}A");
    let unescaped = escape_decode_with_control(&escaped, &control)
        .unwrap()
        .into_uncontrolled()
        .unwrap();
    assert_eq!(unescaped, [0x5c, 0, 1, 0xff, 0x7f, b'A']);
    let error = escape_decode_with_control("a\\8", &control).unwrap_err();
    assert_eq!(
        (error.sqlstate(), error.to_string().as_str()),
        (Some("22P02"), "invalid input syntax for type bytea")
    );
}
