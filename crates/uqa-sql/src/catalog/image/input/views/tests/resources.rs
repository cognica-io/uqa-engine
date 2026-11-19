//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{self, Read};

struct Interrupted<'a> {
    input: &'a [u8],
    remaining: usize,
    panic: bool,
    cancel: Option<&'a CancellationToken>,
}

impl Read for Interrupted<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            assert!(!self.panic, "injected image-reader unwind");
            if let Some(token) = self.cancel.take() {
                token.cancel();
                self.remaining = self.input.len();
            } else {
                return Err(io::Error::other("injected image-reader failure"));
            }
        }
        let count = self
            .remaining
            .min(buffer.len())
            .min(self.input.len())
            .min(7);
        buffer[..count].copy_from_slice(&self.input[..count]);
        self.input = &self.input[count..];
        self.remaining -= count;
        Ok(count)
    }
}

#[test]
fn native_view_images_release_catalogs_on_io_cancel_and_unwind() {
    let (_, data) = fixture();
    for offset in [16, 100, data.len() - 1] {
        for mode in 0..3 {
            let memory = MemoryBudget::new(16 << 20);
            let token = CancellationToken::new();
            let mut reader = Interrupted {
                input: &data,
                remaining: offset,
                panic: mode == 2,
                cancel: (mode == 1).then_some(&token),
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ViewDefinitions::read_image(&mut reader, &memory, &token)
            }));
            if mode == 2 {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(memory.used(), 0, "offset {offset}, mode {mode}");
        }
    }
}

#[test]
fn native_view_images_retain_bound_role_leases_through_independent_catalog_capture() {
    use crate::{
        catalog::roles::{identity::RoleBinding, RoleReference},
        plan::{CommandPlan, CtePlanBody},
    };
    let mut value = view(0);
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        crate::compile("WITH q AS (INSERT INTO t VALUES (1) RETURNING a) SELECT * FROM q")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    value.query = *query;
    let CtePlanBody::Command(command) = &mut value.query.ctes[0].body else {
        panic!("command")
    };
    let CommandPlan::Insert(insert) = command.as_mut() else {
        panic!("insert")
    };
    insert.statement_privilege_subject = Some(RoleReference::Bound(std::sync::Arc::new(
        RoleBinding::new("owner".repeat(10_000), 40000, [5; 16]),
    )));
    let mut original = ViewDefinitions::new();
    original.insert(name(0), value).unwrap();
    let scratch = MemoryBudget::new(16 << 20);
    let data = bytes(&capture(&original, &scratch), 127);
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let restored = ViewDefinitions::read_image(&mut data.as_slice(), &memory, &token).unwrap();
    let CtePlanBody::Command(command) = &restored[&name(0)].query.ctes[0].body else {
        panic!("command")
    };
    let CommandPlan::Insert(insert) = command.as_ref() else {
        panic!("insert")
    };
    let Some(RoleReference::Bound(role)) = &insert.statement_privilege_subject else {
        panic!("bound role")
    };
    let role = role.clone();
    let role_bytes = role.reserved_bytes();
    assert!(role_bytes > 50_000);
    let saved = capture(&restored, &scratch);
    assert!(scratch.used() >= memory.used());
    drop(restored);
    std::thread::spawn(move || {
        assert_eq!(bytes(&saved, 63), data);
        drop(saved);
    })
    .join()
    .unwrap();
    assert_eq!(scratch.used(), 0);
    assert_eq!(memory.used(), role_bytes);
    drop(role);
    assert_eq!(memory.used(), 0);
}

fn text(output: &mut Vec<u8>, value: &str) {
    output.extend((value.len() as u64).to_le_bytes());
    output.extend(value.as_bytes());
}

#[test]
fn native_view_images_match_independent_wire_and_reject_invalid_metadata() {
    let mut data = b"UQAVIW01".to_vec();
    data.extend(1u64.to_le_bytes());
    text(&mut data, "public");
    text(&mut data, "saved");
    data.extend(RoleIdentity::BOOTSTRAP.oid.to_le_bytes());
    data.extend(RoleIdentity::BOOTSTRAP.object_id);
    data.push(0); // Absent relation ACL.
    data.extend(0u64.to_le_bytes()); // Empty column ACLs.
    data.push(0); // Absent relation ACL revision.
    data.extend(0u64.to_le_bytes()); // Empty column revisions.
    let object = data.len();
    data.extend([1; 16]);
    data.push(0); // Original absent output-column metadata.
    let persistence = data.len();
    data.push(0); // Permanent.
    data.extend(0u64.to_le_bytes()); // No options.
    let kind = data.len();
    data.push(0); // Ordinary view.
    data.extend(0u64.to_le_bytes()); // No evaluated rows.
    data.extend(0u64.to_le_bytes()); // No materialized column types.
    data.push(0); // Absent materialized collations.
    let populated = data.len();
    data.push(1);
    data.push(0); // Original absent OID quartet.
    data.push(0); // Original absent array type name.
    data.push(0); // Query relation binding.
    data.extend(0u64.to_le_bytes()); // Empty WITH list.
    data.push(2); // VALUES root.
    data.extend(0u64.to_le_bytes()); // Empty rows.
    data.extend(0u64.to_le_bytes()); // Empty query arena.
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let restored = ViewDefinitions::read_image(&mut data.as_slice(), &memory, &token).unwrap();
    let value = &restored[&RelationIdentity::new("public", "saved")];
    assert_eq!(value.object_id, [1; 16]);
    assert!(value.output_columns.is_none());
    assert!(value.catalog_oids.is_none());
    assert_eq!(value.security.role_owner, RoleIdentity::BOOTSTRAP);
    assert!(
        matches!(&value.query.root, RelationalPlan::Values { rows, subqueries } if rows.is_empty() && subqueries.is_empty())
    );
    let scratch = MemoryBudget::new(1 << 20);
    assert_eq!(bytes(&capture(&restored, &scratch), 1), data);
    drop(restored);
    assert_eq!(memory.used(), 0);
    for (offset, tag) in [
        (persistence, 2),
        (persistence, 255),
        (kind, 2),
        (populated, 2),
    ] {
        let mut invalid = data.clone();
        invalid[offset] = tag;
        assert!(ViewDefinitions::read_image(&mut invalid.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    data[object..object + 16].fill(0);
    assert!(ViewDefinitions::read_image(&mut data.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_view_output_bounds_windows_and_scratch_independently_of_cell_payloads() {
    for length in [1, 65_536, 1 << 20] {
        let mut value = view(0);
        value.materialized_rows[0].insert("body".into(), Value::Str("x".repeat(length)));
        let mut original = ViewDefinitions::new();
        original.insert(name(0), value).unwrap();
        let capture_memory = MemoryBudget::new(16 << 20);
        let captured = capture(&original, &capture_memory);
        let memory = MemoryBudget::new(64 << 10);
        let token = CancellationToken::new();
        let mut reader = None;
        let prepare = allocation_counter::measure(|| {
            reader = Some(captured.image_reader(&memory, &token).unwrap());
        });
        assert_eq!(prepare.count_total, 0);
        let mut reader = reader.unwrap();
        let mut buffer = vec![0; 32768];
        let mut emitted = 0;
        loop {
            let count = reader.read(&mut buffer).unwrap();
            assert!(count <= 16384);
            if count == 0 {
                break;
            }
            emitted += count;
        }
        assert!(emitted > length);
        drop(reader);
        assert_eq!(memory.used(), 0);
        assert!(memory.peak() < 32768);
        let mut interrupted = captured.image_reader(&memory, &token).unwrap();
        interrupted.read(&mut [0; 512]).unwrap();
        token.cancel();
        assert!(interrupted.read(&mut buffer).is_err());
        assert_eq!(memory.used(), 0);
        assert!(interrupted.read(&mut buffer).is_err());
    }
}

#[test]
fn native_view_images_reject_duplicate_incarnations_and_invalid_oid_claims() {
    let token = CancellationToken::new();
    for invalid in 0..5 {
        let first = view(0);
        let mut second = view(1);
        match invalid {
            0 => second.object_id = first.object_id,
            1 => second.catalog_oids = first.catalog_oids,
            2 => {
                let oids = second.catalog_oids.as_mut().unwrap();
                oids.row_type = Some(oids.relation);
            }
            3 => second.catalog_oids.as_mut().unwrap().array_type = None,
            4 => second.catalog_oids.as_mut().unwrap().relation = 0,
            _ => unreachable!(),
        }
        let mut source = ViewDefinitions::new();
        source.insert(name(0), first).unwrap();
        source.insert(name(1), second).unwrap();
        let scratch = MemoryBudget::new(8 << 20);
        let data = bytes(&capture(&source, &scratch), 127);
        let memory = MemoryBudget::new(8 << 20);
        assert!(
            ViewDefinitions::read_image(&mut data.as_slice(), &memory, &token).is_err(),
            "variant {invalid}"
        );
        assert_eq!(memory.used(), 0);
    }
}
