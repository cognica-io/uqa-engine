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
