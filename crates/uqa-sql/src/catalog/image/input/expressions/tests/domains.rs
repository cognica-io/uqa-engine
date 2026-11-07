//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{domain::StoredDomain, type_definitions::TypeDefinitions};
use uqa_core::catalog_role::RoleIdentity;

fn text(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend((value.len() as u64).to_le_bytes());
    bytes.extend(value.as_bytes());
}

fn fixture() -> Vec<u8> {
    let mut bytes = b"UQADOM01\x01\0\0\0\0\0\0\0".to_vec();
    text(&mut bytes, "s.d");
    bytes.extend([1; 16]);
    bytes.extend(65536u32.to_le_bytes());
    bytes.push(0);
    text(&mut bytes, "s");
    text(&mut bytes, "d");
    bytes.extend(RoleIdentity::BOOTSTRAP.oid.to_le_bytes());
    bytes.extend(RoleIdentity::BOOTSTRAP.object_id);
    bytes.extend([0, 0]); // Original array name and default ACL are absent.
    text(&mut bytes, "s.d");
    bytes.extend([0, 0, 1, 1]);
    bytes.extend(1u64.to_le_bytes());
    bytes.push(1);
    bytes.push(1);
    text(&mut bytes, "nn");
    bytes.push(1);
    bytes.extend([2; 16]);
    bytes.extend(65537i64.to_le_bytes());
    bytes.push(3);
    text(&mut bytes, "original");
    bytes.push(1);
    text(&mut bytes, "ck");
    bytes.push(1);
    bytes.extend([3; 16]);
    bytes.extend(65538i64.to_le_bytes());
    bytes.extend([0, 18, 1, 3]);
    text(&mut bytes, "value");
    bytes
}

fn encode_domains(domains: &TypeDefinitions<StoredDomain>) -> Vec<u8> {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let captured = domains
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let bytes = collect(captured.image_reader(&memory, &token).unwrap());
    drop(captured);
    assert_eq!(memory.used(), 0);
    bytes
}

#[test]
fn native_domain_input_preserves_literal_metadata_and_captured_generations() {
    let empty = b"UQADOM01\0\0\0\0\0\0\0\0";
    let zero = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let domains =
        TypeDefinitions::<StoredDomain>::read_image(&mut empty.as_slice(), &zero, &token).unwrap();
    assert!(domains.is_empty());
    assert_eq!(encode_domains(&domains), empty);
    drop(domains);
    assert_eq!(zero.used(), 0);
    let bytes = fixture();
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let domains =
        TypeDefinitions::<StoredDomain>::read_image(&mut bytes.as_slice(), &memory, &token)
            .unwrap();
    let domain = domains.get("s.d").unwrap();
    assert_eq!(domain.object_id, [1; 16]);
    assert_eq!(domain.oid, 65536);
    assert_eq!(domain.array_oid, None);
    assert_eq!(domain.array_name, None);
    assert_eq!(domain.usage_acl, None);
    assert_eq!(domain.owner, RoleIdentity::BOOTSTRAP);
    assert_eq!(
        domain.definition.default.as_ref(),
        Some(&Expr::Column("original".into()))
    );
    assert_eq!(domain.definition.checks.len(), 1);
    assert!(!domain.definition.checks[0].validated);
    assert_eq!(
        domain
            .definition
            .not_null
            .as_ref()
            .unwrap()
            .catalog_identity
            .unwrap()
            .oid,
        65537
    );
    assert_eq!(
        domain.definition.checks[0]
            .catalog_identity
            .unwrap()
            .object_id,
        [3; 16]
    );
    assert_eq!(encode_domains(&domains), bytes);
    let captured = domains
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    drop(domains);
    assert!(memory.used() > 0);
    let output = collect(captured.image_reader(&memory, &token).unwrap());
    assert_eq!(output, bytes);
    drop(captured);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_domain_input_rejects_partial_duplicate_and_mismatched_identities() {
    let bytes = fixture();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(16 << 20);
    for length in 0..bytes.len() {
        assert!(TypeDefinitions::<StoredDomain>::read_image(
            &mut &bytes[..length],
            &memory,
            &token
        )
        .is_err());
        assert_eq!(memory.used(), 0, "prefix {length}");
    }
    let mut cases = vec![];
    let mut repeated = bytes.clone();
    repeated[8..16].copy_from_slice(&2u64.to_le_bytes());
    repeated.extend(&bytes[16..]);
    cases.push(repeated);
    let mut zero = bytes.clone();
    let start = zero.windows(16).position(|v| v == [1; 16]).unwrap();
    zero[start..start + 16].fill(0);
    cases.push(zero);
    let mut constraint = bytes.clone();
    let start = constraint.windows(16).position(|v| v == [3; 16]).unwrap();
    constraint[start..start + 16].fill(2);
    cases.push(constraint);
    let mut key = bytes.clone();
    key[24] = b'x';
    cases.push(key);
    let mut trailing = bytes;
    trailing.push(0);
    cases.push(trailing);
    for bytes in cases {
        assert!(TypeDefinitions::<StoredDomain>::read_image(
            &mut bytes.as_slice(),
            &memory,
            &token
        )
        .is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_domain_input_retires_denied_cancelled_and_unwound_construction() {
    let bytes = fixture();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(16 << 20);
    drop(
        TypeDefinitions::<StoredDomain>::read_image(&mut bytes.as_slice(), &memory, &token)
            .unwrap(),
    );
    let peak = memory.peak();
    assert_eq!(memory.used(), 0);
    for limit in [0, 1, peak - 1] {
        let memory = MemoryBudget::new(limit);
        assert_eq!(
            TypeDefinitions::<StoredDomain>::read_image(&mut bytes.as_slice(), &memory, &token)
                .err()
                .unwrap()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(memory.used(), 0);
    }
    let exact = MemoryBudget::new(peak);
    drop(
        TypeDefinitions::<StoredDomain>::read_image(&mut bytes.as_slice(), &exact, &token).unwrap(),
    );
    assert_eq!(exact.used(), 0);
    for failure in [Failure::Io, Failure::Cancel, Failure::Unwind] {
        for stop in [32, 128, bytes.len() - 1, bytes.len()] {
            let token = CancellationToken::new();
            let mut input = Interrupted {
                bytes: &bytes,
                offset: 0,
                stop,
                failure,
                token: &token,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                TypeDefinitions::<StoredDomain>::read_image(&mut input, &memory, &token)
            }));
            assert!(result.is_err() || result.unwrap().is_err());
            assert_eq!(memory.used(), 0);
        }
    }
}
