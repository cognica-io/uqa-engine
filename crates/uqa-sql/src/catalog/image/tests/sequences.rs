//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::RelationPersistence, catalog::security::BoundSequenceSecurity};
use uqa_core::{catalog_sequence::SequencePrivileges, RelationIdentity};

fn singleton(acl: Option<Vec<BoundAclEntry<SequencePrivileges>>>) -> SequenceSecurities {
    SequenceSecurities::try_from_iter([(
        RelationIdentity::new("s", "q"),
        RelationPersistence::Permanent,
        BoundSequenceSecurity {
            role_owner: RoleIdentity::BOOTSTRAP,
            acl,
        },
    )])
    .unwrap()
}

fn encode(source: &SequenceSecurities) -> Vec<u8> {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let captured = source
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    streamed(captured.image_reader(&token).unwrap(), 7)
}

#[test]
fn sequence_authority_image_matches_literal_identity_and_absent_acl() {
    let source = singleton(None);
    let mut expected = b"UQASQA01\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0s\x01\0\0\0\0\0\0\0q\0\x0a\0\0\0\0\0\0\0UQA:role00000010\0".to_vec();
    assert_eq!(encode(&source), expected);
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let restored =
        SequenceSecurities::read_image(&mut expected.as_slice(), &memory, &token).unwrap();
    assert_eq!(source, restored);
    drop(restored);
    assert_eq!(memory.used(), 0);
    *expected.last_mut().unwrap() = 1;
    expected.extend_from_slice(&0_u64.to_le_bytes());
    assert_eq!(encode(&singleton(Some(vec![]))), expected);
    assert_eq!(
        encode(&SequenceSecurities::default()),
        b"UQASQA01\0\0\0\0\0\0\0\0"
    );
}

#[test]
fn sequence_authority_image_preserves_all_grant_bits_and_original_incarnations() {
    let role = RoleIdentity {
        oid: 42,
        object_id: [7; 16],
    };
    let mut entries = vec![];
    for bits in 0_u8..8 {
        entries.push(BoundAclEntry {
            role: (bits % 2 == 0).then_some(role),
            grantor: role,
            privileges: SequencePrivileges {
                select: bits & 1 != 0,
                update: bits & 2 != 0,
                usage: bits & 4 != 0,
            },
            grant_options: SequencePrivileges {
                select: bits & 4 != 0,
                update: bits & 1 != 0,
                usage: bits & 2 != 0,
            },
        });
    }
    let source = singleton(Some(entries));
    let bytes = encode(&source);
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let restored = SequenceSecurities::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert_eq!(source, restored);
    assert_eq!(encode(&restored), bytes);
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn sequence_authority_image_rejects_bad_tags_names_order_and_partial_payloads() {
    let original = encode(&singleton(Some(vec![BoundAclEntry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: SequencePrivileges::ALL,
        grant_options: SequencePrivileges::default(),
    }])));
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(4096);
    let reject = |bytes: &[u8]| {
        assert!(SequenceSecurities::read_image(&mut &*bytes, &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    };
    for end in 0..original.len() {
        reject(&original[..end]);
    }
    for (offset, byte) in [
        (0, b'X'),
        (24, 0),
        (33, 0),
        (34, 2),
        (59, 2),
        (68, 2),
        (original.len() - 2, 8),
        (original.len() - 1, 8),
    ] {
        let mut bytes = original.clone();
        bytes[offset] = byte;
        reject(&bytes);
    }
    let mut trailing = original.clone();
    trailing.push(0);
    reject(&trailing);
    let mut duplicate = original.clone();
    duplicate[8..16].copy_from_slice(&2_u64.to_le_bytes());
    duplicate.extend_from_slice(&original[16..]);
    reject(&duplicate);
}

#[test]
fn sequence_authority_capture_excludes_temporary_members_and_keeps_admission() {
    let mut source = singleton(None);
    source
        .insert(
            RelationIdentity::new("pg_temp_1", "x".repeat(65536)),
            RelationPersistence::Temporary,
            BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP),
        )
        .unwrap();
    assert_eq!(encode(&source), encode(&singleton(None)));
    let bytes = encode(&source);
    let token = CancellationToken::new();
    let native = MemoryBudget::new(4096);
    let restored = SequenceSecurities::read_image(&mut bytes.as_slice(), &native, &token).unwrap();
    let required = native.used();
    for limit in 0..required {
        let memory = MemoryBudget::new(limit);
        assert!(SequenceSecurities::read_image(&mut bytes.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    drop(restored);
    assert_eq!(native.used(), 0);
    token.cancel();
    assert!(matches!(
        SequenceSecurities::read_image(&mut bytes.as_slice(), &native, &token),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(native.used(), 0);
}
