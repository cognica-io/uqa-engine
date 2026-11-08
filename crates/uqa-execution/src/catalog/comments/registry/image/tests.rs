//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::ProductionControl;
use uqa_sql::ast::RelationPersistence;

mod resources;

fn address(object_id: u32, sub_id: i32, class_id: u32) -> CommentAddress {
    CommentAddress {
        object_id,
        sub_id,
        class_id,
    }
}

fn wire(records: &[(CommentAddress, &[u8])]) -> Vec<u8> {
    let mut bytes = b"UQACMT01".to_vec();
    bytes.extend_from_slice(&(records.len() as u64).to_le_bytes());
    for (address, text) in records {
        bytes.extend_from_slice(&address.object_id.to_le_bytes());
        bytes.extend_from_slice(&address.sub_id.to_le_bytes());
        bytes.extend_from_slice(&address.class_id.to_le_bytes());
        bytes.extend_from_slice(&(text.len() as u64).to_le_bytes());
        bytes.extend_from_slice(text);
    }
    bytes
}

fn bytes(captured: &CapturedComments) -> Vec<u8> {
    let control = StorageReadControl::with_limit(64 << 10);
    let mut reader = captured.image_reader(&control).unwrap();
    let mut output = Vec::new();
    let mut part = [0; 257];
    loop {
        let count = reader.read(&mut part).unwrap();
        if count == 0 {
            break;
        }
        output.extend_from_slice(&part[..count]);
    }
    assert_eq!(control.memory().used(), 0);
    output
}

#[test]
fn native_comment_images_preserve_addresses_text_and_durable_membership() {
    let native = MemoryBudget::new(1 << 20);
    let capture = MemoryBudget::new(1 << 20);
    let restored = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let rows = [
        (address(7, 0, 1247), "original 한글\n"),
        (address(7, 0, 1259), "relation"),
        (address(7, 3, 1259), "column"),
    ];
    let mut registry = CommentRegistry::with_budget(&native);
    for (address, text) in rows {
        registry
            .insert(address, text, RelationPersistence::Permanent)
            .unwrap();
    }
    registry
        .insert(
            address(8, 0, 1259),
            "private",
            RelationPersistence::Temporary,
        )
        .unwrap();
    let captured = registry
        .capture(&ProductionControl::new(&capture, &token, &token))
        .unwrap();
    registry
        .insert(rows[0].0, "replacement", RelationPersistence::Permanent)
        .unwrap();
    drop(registry);
    let expected = wire(&rows.map(|(address, text)| (address, text.as_bytes())));
    assert_eq!(bytes(&captured), expected);
    let decoded = CommentRegistry::read_image(&mut &expected[..], &restored, &token).unwrap();
    assert_eq!(decoded.len(), rows.len());
    for (address, text) in rows {
        assert_eq!(decoded.get(&address).map(String::as_str), Some(text));
        assert!(!decoded.is_temporary(&address));
    }
    drop(captured);
    assert_eq!(capture.used(), 0);
    assert_eq!(native.used(), 0);
    let captured = decoded
        .capture(&ProductionControl::new(&capture, &token, &token))
        .unwrap();
    drop(decoded);
    assert_eq!(bytes(&captured), expected);
    drop(captured);
    assert_eq!(capture.used(), 0);
    assert_eq!(restored.used(), 0);
}

#[test]
fn native_comment_images_reject_partial_malformed_and_unadmitted_input() {
    let token = CancellationToken::new();
    let first = address(1, 0, 1259);
    let second = address(2, 0, 1259);
    let complete = wire(&[(first, b"one"), (second, b"two")]);
    let memory = MemoryBudget::new(1 << 20);
    for end in 0..complete.len() {
        assert!(CommentRegistry::read_image(&mut &complete[..end], &memory, &token).is_err());
        assert_eq!(memory.used(), 0, "prefix {end}");
    }
    let invalid = [
        wire(&[(first, b"one"), (first, b"duplicate")]),
        wire(&[(second, b"two"), (first, b"one")]),
        wire(&[(address(0, 0, 1259), b"text")]),
        wire(&[(address(1, -1, 1259), b"text")]),
        wire(&[(address(1, 0, 0), b"text")]),
        wire(&[(first, b"")]),
        wire(&[(first, b"bad\0text")]),
        wire(&[(first, b"\xff")]),
        [complete.as_slice(), &[0]].concat(),
        [b"BADIMAGE".as_slice(), &complete[8..]].concat(),
    ];
    for bytes in invalid {
        assert!(CommentRegistry::read_image(&mut &bytes[..], &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    let denied = MemoryBudget::new(0);
    assert_eq!(
        CommentRegistry::read_image(&mut &complete[..], &denied, &token)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(denied.used(), 0);
    assert!(
        CommentRegistry::read_image(&mut &wire(&[])[..], &denied, &token)
            .unwrap()
            .is_empty()
    );
    drop(CommentRegistry::read_image(&mut &complete[..], &memory, &token).unwrap());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_comment_image_output_has_bounded_workspace_and_terminal_failure() {
    let token = CancellationToken::new();
    let capture = MemoryBudget::new(8 << 20);
    let mut registry = CommentRegistry::new();
    registry
        .insert(
            address(1, 0, 1259),
            &"x".repeat(4 << 20),
            RelationPersistence::Permanent,
        )
        .unwrap();
    let captured = registry
        .capture(&ProductionControl::new(&capture, &token, &token))
        .unwrap();
    drop(registry);
    let control = StorageReadControl::with_limit(4096);
    let mut output = vec![0; 32768];
    let allocation = allocation_counter::measure(|| {
        let mut reader = captured.image_reader(&control).unwrap();
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        let mut total = 0;
        loop {
            let count = reader.read(&mut output).unwrap();
            assert!(count <= 16384);
            if count == 0 {
                break;
            }
            total += count;
        }
        assert_eq!(total, (4 << 20) + 36);
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(control.memory().used(), 0);
    let mut reader = captured.image_reader(&control).unwrap();
    assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
    control.cancellation().cancel();
    assert!(reader.read(&mut [0; 1]).is_err());
    assert_eq!(control.memory().used(), 0);
    control.cancellation().reset();
    assert!(reader.read(&mut [0; 1]).is_err());
    drop(reader);
    drop(captured);
    assert_eq!(capture.used(), 0);
}

#[test]
fn native_comment_image_output_rejects_invalid_native_entries() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    for (address, text) in [
        (address(0, 0, 1259), "invalid address"),
        (address(1, 0, 1259), ""),
        (address(1, 0, 1259), "invalid\0description"),
    ] {
        let mut registry = CommentRegistry::new();
        registry
            .insert(address, text, RelationPersistence::Permanent)
            .unwrap();
        let captured = registry
            .capture(&ProductionControl::new(&memory, &token, &token))
            .unwrap();
        let control = StorageReadControl::with_limit(4096);
        let mut reader = captured.image_reader(&control).unwrap();
        assert!(reader.read(&mut [0; 256]).is_err());
        assert_eq!(control.memory().used(), 0);
        assert!(reader.read(&mut [0; 1]).is_err());
    }
    assert_eq!(memory.used(), 0);
}
