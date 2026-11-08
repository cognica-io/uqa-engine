//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn properties() -> MemoryTableProperties {
    MemoryTableProperties {
        object_id: [0x11; 16],
        storage_generation: [0x22; 16],
        catalog_oids: Some(MemoryRelationIdentities {
            relation: 42,
            row_type: None,
            array_type: Some(43),
            rule: Some(44),
        }),
        persistence: 1,
        on_commit: 0,
        columns_declared: true,
        maps_integer_keys: false,
        statistics: MemoryTableStatistics {
            loaded: true,
            dirty: true,
        },
        next_id: 1_u128 << 64,
    }
}

fn wire() -> Vec<u8> {
    include_str!("metadata.hex")
        .trim()
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn metadata<'a>(fields: &'a [String], analyzer: &'a [u8]) -> MemoryTableMetadata<'a> {
    MemoryTableMetadata {
        properties: properties(),
        fts_fields: fields,
        analyzer,
        row_type_array_name: Some(""),
    }
}

fn encode(value: MemoryTableMetadata<'_>, window: usize) -> Vec<u8> {
    let control = StorageReadControl::with_limit(4096);
    let mut reader = memory_table_metadata_reader(value, &control).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = vec![0; window];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        assert!(count <= 16 * 1024);
        bytes.extend_from_slice(&buffer[..count]);
    }
    drop(reader);
    assert_eq!(control.memory().used(), 0);
    bytes
}

#[test]
fn memory_table_metadata_preserves_independent_wire_and_native_optional_values() {
    let fields = vec!["body".into(), "한글".into()];
    for window in [1, 7, 8192, 32768] {
        assert_eq!(encode(metadata(&fields, &[0, 1, 255]), window), wire());
    }
    let control = StorageReadControl::with_limit(4096);
    let decoded = read_memory_table_metadata(&mut wire().as_slice(), &control).unwrap();
    assert_eq!(decoded.properties, properties());
    assert!(decoded
        .fts_fields
        .iter()
        .map(|field| field.as_str())
        .eq(["body", "한글"]));
    assert_eq!(**decoded.analyzer, [0, 1, 255]);
    assert_eq!(
        decoded
            .row_type_array_name
            .as_ref()
            .map(|name| name.as_str()),
        Some("")
    );
    drop(decoded);
    assert_eq!(control.memory().used(), 0);
    for name in [None, Some(""), Some("saved_array")] {
        let mut value = metadata(&[], &[]);
        value.properties.catalog_oids = None;
        value.properties.next_id = u128::MAX;
        value.row_type_array_name = name;
        let bytes = encode(value, 1);
        let decoded = read_memory_table_metadata(&mut bytes.as_slice(), &control).unwrap();
        assert_eq!(decoded.properties.catalog_oids, None);
        assert_eq!(decoded.properties.next_id, u128::MAX);
        assert_eq!(
            decoded
                .row_type_array_name
                .as_ref()
                .map(|name| name.as_str()),
            name
        );
        assert!(decoded.fts_fields.is_empty());
        assert!(decoded.analyzer.is_empty());
        drop(decoded);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn memory_table_metadata_rejects_truncation_tags_trailing_bytes_and_unadmitted_counts() {
    let bytes = wire();
    let control = StorageReadControl::with_limit(4096);
    for length in 0..bytes.len() {
        assert!(
            read_memory_table_metadata(&mut &bytes[..length], &control).is_err(),
            "prefix {length}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    for position in [0, 40, 45, 46, 51, 56, 57, 58, 59, 60, 61, bytes.len() - 9] {
        let mut invalid = bytes.clone();
        invalid[position] = 255;
        assert!(
            read_memory_table_metadata(&mut invalid.as_slice(), &control).is_err(),
            "tag {position}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(read_memory_table_metadata(&mut trailing.as_slice(), &control).is_err());
    let mut oversized = bytes;
    oversized[78..86].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(
        read_memory_table_metadata(&mut oversized.as_slice(), &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_table_metadata_streams_large_borrowed_configuration_without_payload_scratch() {
    let fields = vec!["body".repeat(16384), String::new()];
    let analyzer = vec![37; 4 << 20];
    let control = StorageReadControl::with_limit(4096);
    let mut reader = memory_table_metadata_reader(metadata(&fields, &analyzer), &control).unwrap();
    let retained = control.memory().used();
    let mut bytes = 0;
    let mut buffer = vec![0; 32768];
    let usage = allocation_counter::measure(|| loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        assert!(count <= 16 * 1024);
        bytes += count;
    });
    assert_eq!(usage.count_total, 0);
    assert!(retained < 4096);
    assert_eq!(bytes, 86 + 16 + fields[0].len() + 8 + analyzer.len() + 9);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_table_metadata_cancellation_and_unwind_release_private_payloads() {
    struct Failing<'a> {
        bytes: &'a [u8],
        read: usize,
    }
    impl Read for Failing<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            assert!(self.read < 100, "input unwound after field admission");
            let count = self.bytes.read(output)?;
            self.read += count;
            Ok(count)
        }
    }
    let control = StorageReadControl::with_limit(4096);
    let mut reader = memory_table_metadata_reader(metadata(&[], &[]), &control).unwrap();
    reader.read(&mut [0; 7]).unwrap();
    control.cancellation().cancel();
    assert!(reader.read(&mut [0; 7]).is_err());
    assert_eq!(control.memory().used(), 0);
    assert!(reader.read(&mut [0; 7]).is_err());

    let control = StorageReadControl::with_limit(4096);
    let bytes = wire();
    let mut input = Failing {
        bytes: &bytes,
        read: 0,
    };
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        read_memory_table_metadata(&mut input, &control).unwrap();
    }))
    .is_err());
    assert_eq!(control.memory().used(), 0);
    let denied = StorageReadControl::with_limit(0);
    assert!(matches!(
        read_memory_table_metadata(&mut bytes.as_slice(), &denied),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(denied.memory().used(), 0);
}
