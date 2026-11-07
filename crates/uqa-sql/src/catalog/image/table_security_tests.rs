//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::catalog::{
    roles::RoleIdentity,
    security::table_binding::{BoundTableAclEntry, CapturedTablePrivileges, CapturedTableSecurity},
};
use uqa_core::{catalog_acl::TablePrivileges, memory::MemoryBudget, CancellationToken};

fn fixture() -> CapturedTableSecurity {
    let owner = RoleIdentity {
        oid: 65536,
        object_id: [1; 16],
    };
    let entry = BoundTableAclEntry {
        role: None,
        grantor: owner,
        privileges: TablePrivileges {
            select: true,
            ..TablePrivileges::default()
        },
        grant_options: TablePrivileges::default(),
    };
    CapturedTableSecurity {
        privileges: CapturedTablePrivileges::from_image_fields(
            owner,
            Some(vec![entry.clone()]),
            vec![("x".into(), vec![entry]), ("z".into(), vec![])],
        ),
        relation_revision: Some([2; 16]),
        column_revisions: vec![("x".into(), [3; 16])],
    }
}

fn encode(value: &CapturedTableSecurity, size: usize) -> Vec<u8> {
    let token = CancellationToken::new();
    let mut reader = value.image_reader(&token).unwrap();
    let mut output = Vec::new();
    let mut bytes = vec![0; size];
    loop {
        let count = reader.read(&mut bytes).unwrap();
        assert!(count <= 16384);
        if count == 0 {
            break;
        }
        output.extend_from_slice(&bytes[..count]);
    }
    assert_eq!(reader.read(&mut bytes).unwrap(), 0);
    output
}

fn text(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend((value.len() as u64).to_le_bytes());
    bytes.extend(value.as_bytes());
}

#[test]
fn native_table_authority_images_match_independent_acl_and_revision_bytes() {
    let value = fixture();
    let mut expected = b"UQATSA01\x01\0\0\0\0\0\0\0".to_vec();
    expected.extend(65536i64.to_le_bytes());
    expected.extend([1; 16]);
    expected.push(1);
    expected.extend([2; 16]);
    expected.extend(2u64.to_le_bytes());
    expected.extend(1u64.to_le_bytes());
    expected.push(1);
    expected.extend(1u64.to_le_bytes());
    let mut acl = vec![0];
    acl.extend(65536i64.to_le_bytes());
    acl.extend([1; 16]);
    acl.extend([1, 0]);
    expected.extend(&acl);
    text(&mut expected, "x");
    expected.extend(1u64.to_le_bytes());
    expected.extend(&acl);
    text(&mut expected, "z");
    expected.extend(0u64.to_le_bytes());
    text(&mut expected, "x");
    expected.extend([3; 16]);
    for size in [1, 31, 4096, 20000] {
        assert_eq!(encode(&value, size), expected);
    }
    let memory = MemoryBudget::new(1 << 20);
    let saved = CapturedTableSecurity::read_image(
        &mut expected.as_slice(),
        &memory,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(*saved, value);
    assert_eq!(encode(&saved, 13), expected);
    drop(saved);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_table_authority_images_preserve_optional_acl_paths_and_all_privileges() {
    for mask in 0u16..256 {
        let mut value = fixture();
        let privileges = TablePrivileges {
            select: mask & 1 != 0,
            insert: mask & 2 != 0,
            update: mask & 4 != 0,
            delete: mask & 8 != 0,
            truncate: mask & 16 != 0,
            references: mask & 32 != 0,
            trigger: mask & 64 != 0,
            maintain: mask & 128 != 0,
        };
        let entry = BoundTableAclEntry {
            role: Some(RoleIdentity {
                oid: 65537,
                object_id: [4; 16],
            }),
            grantor: value.privileges.role_owner,
            privileges,
            grant_options: privileges,
        };
        let acl = match mask % 3 {
            0 => None,
            1 => Some(vec![]),
            _ => Some(vec![entry.clone()]),
        };
        value.privileges = CapturedTablePrivileges::from_image_fields(
            value.privileges.role_owner,
            acl,
            vec![("x".into(), vec![entry])],
        );
        if mask & 1 == 0 {
            value.relation_revision = None;
        }
        if mask & 2 == 0 {
            value.column_revisions.clear();
        }
        let bytes = encode(&value, 37);
        let memory = MemoryBudget::new(1 << 20);
        let saved = CapturedTableSecurity::read_image(
            &mut bytes.as_slice(),
            &memory,
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(*saved, value);
        drop(saved);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_table_authority_input_rejects_invalid_order_and_partial_or_denied_images() {
    let bytes = encode(&fixture(), 127);
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    for length in 0..bytes.len() {
        assert!(CapturedTableSecurity::read_image(&mut &bytes[..length], &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(CapturedTableSecurity::read_image(&mut trailing.as_slice(), &memory, &token).is_err());
    for (start, end) in [(8, 16), (57, 65), (65, 73)] {
        let mut invalid = bytes.clone();
        invalid[start..end].fill(255);
        assert!(
            CapturedTableSecurity::read_image(&mut invalid.as_slice(), &memory, &token).is_err()
        );
        assert_eq!(memory.used(), 0);
    }
    for position in [0, 40, 73] {
        let mut invalid = bytes.clone();
        invalid[position] = 255;
        assert!(
            CapturedTableSecurity::read_image(&mut invalid.as_slice(), &memory, &token).is_err()
        );
        assert_eq!(memory.used(), 0);
    }
    let mut value = fixture();
    value.column_revisions = vec![("x".into(), [3; 16]), ("x".into(), [4; 16])];
    assert!(CapturedTableSecurity::read_image(
        &mut encode(&value, 127).as_slice(),
        &memory,
        &token
    )
    .is_err());
    value.privileges = CapturedTablePrivileges::from_image_fields(
        value.privileges.role_owner,
        None,
        vec![("z".into(), vec![]), ("a".into(), vec![])],
    );
    value.column_revisions.clear();
    assert!(CapturedTableSecurity::read_image(
        &mut encode(&value, 127).as_slice(),
        &memory,
        &token
    )
    .is_err());
    let ample = MemoryBudget::new(1 << 20);
    drop(CapturedTableSecurity::read_image(&mut bytes.as_slice(), &ample, &token).unwrap());
    for limit in [0, 1, ample.peak() - 1] {
        let denied = MemoryBudget::new(limit);
        assert_eq!(
            CapturedTableSecurity::read_image(&mut bytes.as_slice(), &denied, &token)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(denied.used(), 0);
    }
    let exact = MemoryBudget::new(ample.peak());
    drop(CapturedTableSecurity::read_image(&mut bytes.as_slice(), &exact, &token).unwrap());
    assert_eq!(exact.used(), 0);
}

#[test]
fn native_table_authority_output_borrows_large_columns_and_keeps_terminal_cancellation() {
    let mut value = fixture();
    let name = "x".repeat(8 << 20);
    value.privileges = CapturedTablePrivileges::from_image_fields(
        value.privileges.role_owner,
        None,
        vec![(name, vec![])],
    );
    let token = CancellationToken::new();
    let mut buffer = vec![0; 20000];
    let mut length = 0;
    let observed = allocation_counter::measure(|| {
        let mut reader = value.image_reader(&token).unwrap();
        loop {
            let count = reader.read(&mut buffer).unwrap();
            assert!(count <= 16384);
            if count == 0 {
                break;
            }
            length += count;
        }
    });
    assert!(length > 8 << 20);
    assert_eq!(observed.bytes_total, 0);
    let mut reader = value.image_reader(&token).unwrap();
    reader.read(&mut [0; 100]).unwrap();
    token.cancel();
    assert!(reader.read(&mut buffer).is_err());
    assert!(reader.read(&mut []).is_err());
}

#[test]
fn native_table_authority_input_retires_io_cancellation_and_unwinding() {
    struct Fault<'a> {
        bytes: &'a [u8],
        offset: usize,
        stop: usize,
        mode: u8,
        token: &'a CancellationToken,
    }
    impl std::io::Read for Fault<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            if self.offset >= self.stop {
                match self.mode {
                    0 => return Err(std::io::Error::other("authority input failure")),
                    1 => self.token.cancel(),
                    _ => panic!("authority input unwind"),
                }
            }
            let count = output
                .len()
                .min(self.bytes.len() - self.offset)
                .min(self.stop.saturating_sub(self.offset).max(1));
            output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
            self.offset += count;
            Ok(count)
        }
    }
    let bytes = encode(&fixture(), 31);
    let memory = MemoryBudget::new(1 << 20);
    for mode in 0..3 {
        for stop in [17, bytes.len() / 2, bytes.len()] {
            let token = CancellationToken::new();
            let mut source = Fault {
                bytes: &bytes,
                offset: 0,
                stop,
                mode,
                token: &token,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                CapturedTableSecurity::read_image(&mut source, &memory, &token)
            }));
            assert!(result.is_err() || result.unwrap().is_err());
            assert_eq!(memory.used(), 0);
        }
    }
}
