//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn composite_image_matches_literal_type_and_attribute_number_bytes() {
    let mut bytes = b"UQACMP01\x01\0\0\0\0\0\0\0\x08\0\0\0\0\0\0\0public.c".to_vec();
    bytes.extend_from_slice(&[10; 16]);
    bytes.extend_from_slice(b"\0\0\x01\0\x01\0\x01\0\x02\0\x01\0\x02\0\0\0\0\0\0\0_c\x06\0\0\0\0\0\0\0public\x01\0\0\0\0\0\0\0c\x0a\0\0\0\0\0\0\0UQA:role00000010\x01\0\0\0\0\0\0\0\0");
    bytes.extend_from_slice(b"\x01\0\0\0\0\0\0\0a\0\0\x03\0\0\x01");
    assert_round_trip(&singleton("public.c", composite()), &bytes);
}

#[test]
fn range_image_matches_literal_support_and_independent_multirange_identity_bytes() {
    let mut bytes = b"UQARNG01\x01\0\0\0\0\0\0\0\x08\0\0\0\0\0\0\0public.r".to_vec();
    bytes.extend_from_slice(&[11; 16]);
    bytes.extend_from_slice(b"\x03\0\x01\0\x04\0\x01\0\x02\0\0\0\0\0\0\0_r\x06\0\0\0\0\0\0\0public\x01\0\0\0\0\0\0\0r\x0a\0\0\0\0\0\0\0UQA:role00000010\xba\x07\0\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\0\x01");
    bytes.extend_from_slice(&[12; 16]);
    bytes.extend_from_slice(
        b"\x05\0\x01\0\x06\0\x01\0\x02\0\0\0\0\0\0\0_m\x05\0\0\0\0\0\0\0moved\x01\0\0\0\0\0\0\0m",
    );
    assert_round_trip(&singleton("public.r", range()), &bytes);
}

#[test]
fn empty_structured_type_images_are_complete_empty_native_registries() {
    assert_round_trip(
        &NamedDefinitions::<StoredComposite>::new(),
        b"UQACMP01\0\0\0\0\0\0\0\0",
    );
    assert_round_trip(
        &NamedDefinitions::<StoredRange>::new(),
        b"UQARNG01\0\0\0\0\0\0\0\0",
    );
}
