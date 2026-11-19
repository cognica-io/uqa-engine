//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn enum_image_matches_literal_identities_keys_and_float4_bits() {
    // Explicit version-one bytes: no encoder helpers, regenerated identities or normalized float values.
    let mut expected = b"UQAENU01\x01\0\0\0\0\0\0\0\x08\0\0\0\0\0\0\0public.e".to_vec();
    expected.extend_from_slice(&[7; 16]);
    expected.extend_from_slice(b"\0\0\x01\0\x01\0\x01\0\x02\0\0\0\0\0\0\0_e\x06\0\0\0\0\0\0\0public\x01\0\0\0\0\0\0\0e\x0a\0\0\0\0\0\0\0UQA:role00000010\x02\0\0\0\0\0\0\0\0");
    expected.extend_from_slice(b"\x02\0\x01\0\x01\0\0\0\0\0\0\0\x40\0\0\0\0\0\0\0\0\0\0\0\x80");
    expected.extend_from_slice(
        b"\x03\0\x01\0\x01\0\0\0\0\0\0\0\x80\x02\0\0\0\0\0\0\0\xc3\xa9\0\0\xc0\x3f",
    );
    assert_round_trip(&singleton("public.e", enum_value()), &expected);
}

#[test]
fn shell_image_matches_literal_identity_and_explicit_empty_acl() {
    let mut expected = b"UQASHL01\x01\0\0\0\0\0\0\0\x08\0\0\0\0\0\0\0public.s".to_vec();
    expected.extend_from_slice(&[8; 16]);
    expected.extend_from_slice(b"\x04\0\x01\0\x06\0\0\0\0\0\0\0public\x01\0\0\0\0\0\0\0s\x0a\0\0\0\0\0\0\0UQA:role00000010\x01\0\0\0\0\0\0\0\0");
    assert_round_trip(&singleton("public.s", shell_value()), &expected);
}

#[test]
fn foreign_server_image_matches_literal_optional_fields_and_declaration_order() {
    let mut expected = b"UQAFSR01\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0s\x01\0\0\0\0\0\0\0s\x0a\0\0\0\0\0\0\0memory_fdw\x05\0\x01\0".to_vec();
    expected.extend_from_slice(&[9; 16]);
    expected.extend_from_slice(b"\x0a\0\0\0\0\0\0\0UQA:role00000010\x01\0\0\0\0\0\0\0\0\0\x01\x01\0\0\0UQA native FDW\0\x01\x02\0\0\0\0\0\0\0\x01\x02\0\0\0\0\0\0\0");
    expected.extend_from_slice(
        b"\x01\0\0\0\0\0\0\0a\x02\0\0\0\0\0\0\0\xce\xb1\x01\0\0\0\0\0\0\0z\0\0\0\0\0\0\0\0",
    );
    expected.extend_from_slice(b"\x01\0\0\0\0\0\0\0z\x01\0\0\0\0\0\0\0a");
    assert_round_trip(&singleton("s", server_value()), &expected);
}

#[test]
fn graph_oid_image_matches_literal_nested_companions_and_column_keys() {
    let mut expected =
        b"UQAGOI01\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0g\x06\0\x01\0\x07\0\x01\0\x01\0\0\0\0\0\0\0"
            .to_vec();
    expected.extend_from_slice(b"\x11\0\0\0\x08\0\x01\0\x09\0\x01\0\x01\x0a\0\x01\0\x01\x0b\0\x01\0\x01\x0c\0\x01\0\x0d\0\x01\0\x0e\0\x01\0\x11\0\x01\0\x12\0\x01\0\x01\x13\0\x01\0\x14\0\x01\0\x01\x15\0\x01\0\x16\0\x01\0\x17\0\x01\0\x02\0\0\0\0\0\0\0");
    expected.extend_from_slice(
        b"\x02\0\0\0\0\0\0\0id\x0f\0\x01\0\x0a\0\0\0\0\0\0\0properties\x10\0\x01\0",
    );
    assert_round_trip(&singleton("g", graph_value()), &expected);
}
