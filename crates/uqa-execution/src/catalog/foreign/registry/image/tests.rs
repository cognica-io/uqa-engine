//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::catalog::roles::RoleIdentity;

mod native;

fn key() -> RelationIdentity {
    RelationIdentity::new("Quoted.Schema", "Foreign.Name")
}
fn table() -> StoredForeignTable {
    crate::catalog::foreign::capture::tests::definition()
}
fn authority() -> BoundTableSecurity {
    let mut value = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    value.acl = Some(Vec::new());
    value.acl_revisions.relation = Some([7; 16]);
    value.column_acls.insert("payload".into(), Vec::new());
    value
        .acl_revisions
        .columns
        .insert("payload".into(), [8; 16]);
    value
}
fn collect(value: &CapturedForeignCatalog, width: usize) -> Vec<u8> {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut source = value.image_reader(&control).unwrap();
    let mut bytes = Vec::new();
    let mut output = vec![0; width];
    loop {
        let count = source.read(&mut output).unwrap();
        assert!(count <= 16384);
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&output[..count]);
    }
    assert_eq!(control.memory().used(), 0);
    bytes
}
fn image() -> Vec<u8> {
    let mut source = ForeignCatalog::default();
    source.insert(key(), table(), authority()).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let saved = source
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    collect(&saved, 29)
}

#[test]
fn native_foreign_images_validate_paired_membership_and_preserve_empty_roots() {
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut left = ForeignCatalog::with_budget(&memory);
    assert_eq!(
        collect(&left.capture(&control).unwrap(), 7),
        b"UQAFCT01\0\0\0\0\0\0\0\0"
    );
    left.insert(key(), table(), authority()).unwrap();
    let mut right = ForeignCatalog::with_budget(&memory);
    right
        .insert(RelationIdentity::new("other", "name"), table(), authority())
        .unwrap();
    let (left, left_lease) = left.capture(&control).unwrap().into_parts();
    let (right, right_lease) = right.capture(&control).unwrap().into_parts();
    let malformed = CapturedForeignCatalog {
        tables: left.tables,
        security: right.security,
    };
    let output = StorageReadControl::with_limit(64 << 10);
    let mut source = malformed.image_reader(&output).unwrap();
    assert!(source.read(&mut [0; 4096]).is_err());
    assert!(source.read(&mut [0; 1]).is_err());
    assert_eq!(output.memory().used(), 0);
    drop((left_lease, right_lease));
}

#[test]
fn native_foreign_images_preserve_original_paired_fields_and_independent_generations() {
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut source = ForeignCatalog::with_budget(&memory);
    source.insert(key(), table(), authority()).unwrap();
    let mut temporary = table();
    temporary.persistence = RelationPersistence::Temporary;
    source
        .insert(
            RelationIdentity::new("pg_temp", "private"),
            temporary,
            authority(),
        )
        .unwrap();
    let saved = source.capture(&control).unwrap();
    source.remove(&key()).unwrap();
    drop(source);
    let bytes = collect(&saved, 1);
    assert_eq!(bytes, collect(&saved, 32768));
    assert_eq!(&bytes[..16], b"UQAFCT01\x01\0\0\0\0\0\0\0");
    let decoded_control = StorageReadControl::with_limit(8 << 20);
    let decoded = ForeignCatalog::read_image(&mut bytes.as_slice(), &decoded_control).unwrap();
    assert_eq!(decoded.tables().len(), 1);
    let expected = table();
    let actual = decoded.tables().get(&key()).unwrap();
    assert_eq!(actual.name, expected.name);
    assert_eq!(actual.server_name, expected.server_name);
    assert_eq!(
        actual.schema_json().unwrap(),
        expected.schema_json().unwrap()
    );
    assert_eq!(actual.options, expected.options);
    assert!(actual.columns[1]
        .missing_value
        .as_ref()
        .unwrap()
        .has_same_representation(expected.columns[1].missing_value.as_ref().unwrap()));
    assert_eq!(decoded.security().get(&key()).unwrap(), &authority());
    let retained = decoded.clone();
    drop(decoded);
    let recaptured = retained.capture(&control).unwrap();
    drop(retained);
    assert_eq!(collect(&recaptured, 4096), bytes);
    assert!(decoded_control.memory().used() > 0);
    drop(recaptured);
    assert_eq!(decoded_control.memory().used(), 0);
    drop(saved);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_foreign_images_reject_every_truncation_trailing_data_and_denied_input() {
    let bytes = image();
    let control = StorageReadControl::with_limit(1 << 20);
    for end in 0..bytes.len() {
        assert!(
            ForeignCatalog::read_image(&mut &bytes[..end], &control).is_err(),
            "prefix {end}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(ForeignCatalog::read_image(&mut extra.as_slice(), &control).is_err());
    assert_eq!(control.memory().used(), 0);
    let mut repeated = bytes.clone();
    repeated[8..16].copy_from_slice(&2u64.to_le_bytes());
    repeated.extend_from_slice(&bytes[16..]);
    assert_eq!(
        ForeignCatalog::read_image(&mut repeated.as_slice(), &control)
            .unwrap_err()
            .sqlstate(),
        Some("XX001")
    );
    assert_eq!(control.memory().used(), 0);
    let persistence = 16 + 8 + key().schema.len() + 8 + key().name.len() + 8 + table().name.len();
    for offset in [0, persistence, persistence + 17] {
        let mut invalid = bytes.clone();
        invalid[offset] = 2;
        assert!(ForeignCatalog::read_image(&mut invalid.as_slice(), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let denied = StorageReadControl::with_limit(32);
    assert_eq!(
        ForeignCatalog::read_image(&mut bytes.as_slice(), &denied)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(denied.memory().used(), 0);
}

#[test]
fn native_foreign_images_preserve_absence_unlogged_persistence_and_reject_invalid_options() {
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut source = ForeignCatalog::with_budget(&memory);
    let mut value = table();
    value.persistence = RelationPersistence::Unlogged;
    value.catalog_oids = None;
    value.row_type_array_name = None;
    value.server_reference = None;
    source.insert(key(), value, authority()).unwrap();
    let bytes = collect(&source.capture(&control).unwrap(), 37);
    let read_control = StorageReadControl::with_limit(1 << 20);
    let restored = ForeignCatalog::read_image(&mut bytes.as_slice(), &read_control).unwrap();
    let restored = restored.tables().get(&key()).unwrap();
    assert_eq!(restored.persistence, RelationPersistence::Unlogged);
    assert!(
        restored.catalog_oids.is_none()
            && restored.row_type_array_name.is_none()
            && restored.server_reference.is_none()
    );
    for order in [
        vec!["a".into(), "a".into()],
        vec!["a".into()],
        vec!["a".into(), "absent".into()],
    ] {
        let mut value = table();
        value.option_order = order;
        source.insert(key(), value, authority()).unwrap();
        let bytes = collect(&source.capture(&control).unwrap(), 101);
        let control = StorageReadControl::with_limit(1 << 20);
        assert!(ForeignCatalog::read_image(&mut bytes.as_slice(), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn native_foreign_image_output_has_fixed_payload_workspace_and_terminal_cancellation() {
    let mut source = ForeignCatalog::default();
    source.insert(key(), table(), authority()).unwrap();
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let production = ProductionControl::new(&memory, &token, &token);
    let small = source.capture(&production).unwrap();
    let mut value = table();
    value.options.insert("a".into(), "x".repeat(4 << 20));
    source.insert(key(), value, authority()).unwrap();
    let saved = source.capture(&production).unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let mut output = vec![0; 32768];
    let mut measure = |saved: &CapturedForeignCatalog| {
        let mut count = 0;
        let allocation = allocation_counter::measure(|| {
            let mut source = saved.image_reader(&control).unwrap();
            loop {
                let read = source.read(&mut output).unwrap();
                if read == 0 {
                    break;
                }
                count += read;
            }
        });
        (count, allocation)
    };
    let (small_bytes, small_allocation) = measure(&small);
    let (large_bytes, large_allocation) = measure(&saved);
    assert!(small_bytes < 64 << 10 && large_bytes > 4 << 20);
    assert_eq!(small_allocation, large_allocation);
    assert!(
        large_allocation.bytes_max < 32 << 10,
        "{large_allocation:?}"
    );
    assert_eq!(control.memory().used(), 0);
    let mut reader = saved.image_reader(&control).unwrap();
    reader.read(&mut output).unwrap();
    control.cancellation().cancel();
    assert!(reader.read(&mut output).is_err());
    assert_eq!(control.memory().used(), 0);
    assert!(reader.read(&mut output).is_err());
}

#[test]
fn native_foreign_image_input_retires_partial_owners_after_io_cancellation_and_unwind() {
    struct Interrupted<'a> {
        bytes: &'a [u8],
        calls: usize,
    }
    impl Read for Interrupted<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls.is_multiple_of(3) {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            self.bytes.read(&mut output[..1])
        }
    }
    struct Fault<'a> {
        bytes: &'a [u8],
        remaining: usize,
        unwind: bool,
    }
    impl Read for Fault<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                assert!(!self.unwind, "injected image I/O");
                return Err(std::io::Error::other("injected image I/O"));
            }
            let limit = output.len().min(self.remaining);
            let count = self.bytes.read(&mut output[..limit])?;
            self.remaining -= count;
            Ok(count)
        }
    }
    let bytes = image();
    let control = StorageReadControl::with_limit(1 << 20);
    drop(
        ForeignCatalog::read_image(
            &mut Interrupted {
                bytes: &bytes,
                calls: 0,
            },
            &control,
        )
        .unwrap(),
    );
    assert_eq!(control.memory().used(), 0);
    for remaining in [bytes.len() / 2, bytes.len() - 4] {
        assert!(ForeignCatalog::read_image(
            &mut Fault {
                bytes: &bytes,
                remaining,
                unwind: false
            },
            &control
        )
        .is_err());
        assert_eq!(control.memory().used(), 0);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ForeignCatalog::read_image(
                &mut Fault {
                    bytes: &bytes,
                    remaining,
                    unwind: true,
                },
                &control,
            )
        }))
        .is_err());
        assert_eq!(control.memory().used(), 0);
    }
    control.cancellation().cancel();
    assert_eq!(
        ForeignCatalog::read_image(&mut bytes.as_slice(), &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_foreign_capture_funds_retained_literal_children_before_and_after_image_input() {
    use uqa_core::Value;
    use uqa_sql::{
        ast::{Expr, RetainedInputLiteral},
        ir::{RetainedConstantValue, RetainedScalarLiteral},
    };
    for check in [false, true] {
        let native = MemoryBudget::new(32 << 20);
        let token = CancellationToken::new();
        let production = ProductionControl::new(&native, &token, &token);
        let value = production
            .copy_value(&Value::Str("x".repeat(4 << 20)))
            .unwrap();
        let value = RetainedConstantValue::from_produced(value, &production).unwrap();
        let value =
            RetainedScalarLiteral::with_control(value, None, None, None, &production).unwrap();
        let expression = Expr::RetainedLiteral(Box::new(RetainedInputLiteral(value)));
        let mut definition = table();
        if check {
            definition.checks[0].expr = expression;
        } else {
            definition.columns[1].default = Some(expression);
        }
        let mut source = ForeignCatalog::with_budget(&native);
        source.insert(key(), definition, authority()).unwrap();
        let denied = MemoryBudget::new(64 << 10);
        let small = ProductionControl::new(&denied, &token, &token);
        assert_eq!(
            source.capture(&small).err().unwrap().sqlstate(),
            Some("53200")
        );
        assert_eq!(denied.used(), 0);
        let saved = source.capture(&production).unwrap();
        assert!(saved.reserved_bytes() > 4 << 20);
        let bytes = collect(&saved, 4096);
        let control = StorageReadControl::with_limit(32 << 20);
        let decoded = ForeignCatalog::read_image(&mut bytes.as_slice(), &control).unwrap();
        assert_eq!(
            decoded.capture(&small).err().unwrap().sqlstate(),
            Some("53200")
        );
        assert_eq!(denied.used(), 0);
        let recaptured = decoded.capture(&production).unwrap();
        assert_eq!(recaptured.reserved_bytes(), saved.reserved_bytes());
        drop((source, saved, decoded));
        assert_eq!(collect(&recaptured, 4096), bytes);
        assert!(control.memory().used() > 4 << 20);
        drop(recaptured);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(native.used(), 0);
    }
}
