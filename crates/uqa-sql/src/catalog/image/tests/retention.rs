//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Read, Write};

#[test]
fn catalog_export_borrows_large_original_names_without_allocation_or_scan() {
    let value = role(&"large_ä".repeat(50000), 42);
    let mut roles = RoleDefinitions::new();
    roles.insert(value.name.clone(), value).unwrap();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(4 << 20);
    let captured = roles
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    roles.clear();
    drop(roles);
    let expected = streamed(captured.image_reader(&token).unwrap(), 4096);
    let mut length = 0;
    let mut buffer = vec![0; 20000];
    let allocations = allocation_counter::measure(|| {
        let mut reader = captured.image_reader(&token).unwrap();
        loop {
            let read = reader.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            assert!(read <= 16384);
            assert_eq!(&buffer[..read], &expected[length..length + read]);
            length += read;
        }
    });
    assert_eq!(length, expected.len());
    assert_eq!(allocations.count_total, 0);
    drop(captured);
    assert_eq!(memory.used(), 0);
}

#[test]
fn imported_catalogs_keep_exact_admission_through_clone_capture_and_mutation() {
    let bytes = encode_roles(&catalog());
    let token = CancellationToken::new();
    let ample = MemoryBudget::new(1 << 20);
    let restored = RoleDefinitions::read_image(&mut bytes.as_slice(), &ample, &token).unwrap();
    let required = ample.peak();
    let retained = ample.used();
    assert!(required >= retained);
    drop(restored);
    assert_eq!(ample.used(), 0);
    let exact = MemoryBudget::new(required);
    let mut restored = RoleDefinitions::read_image(&mut bytes.as_slice(), &exact, &token).unwrap();
    assert_eq!(exact.peak(), required);
    assert_eq!(exact.used(), retained);
    let clone = restored.clone();
    let capture_memory = MemoryBudget::new(1 << 20);
    let saved = restored
        .capture(&ProductionControl::new(&capture_memory, &token, &token))
        .unwrap();
    // Complete captured footprint is the complete imported native payload, without counting key leases twice.
    assert_eq!(capture_memory.used(), retained);
    restored.clear();
    drop(restored);
    assert_eq!(exact.used(), retained);
    drop(clone);
    assert_eq!(exact.used(), retained);
    assert_eq!(streamed(saved.image_reader(&token).unwrap(), 13), bytes);
    drop(saved);
    assert_eq!(exact.used(), 0);
    assert_eq!(capture_memory.used(), 0);
    for limit in [0, 1, 16, 64, retained.saturating_sub(1), required - 1] {
        let memory = MemoryBudget::new(limit);
        let result = RoleDefinitions::read_image(&mut bytes.as_slice(), &memory, &token);
        assert!(result.is_err());
        assert_eq!(result.err().unwrap().sqlstate(), Some("53200"));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn catalog_io_checks_partial_attempts_cancellation_and_retires_failed_output() {
    struct Source<'a> {
        input: &'a [u8],
        token: &'a CancellationToken,
        remaining: usize,
    }
    impl Read for Source<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            assert!(output.len() <= 4096);
            if self.remaining == 0 {
                self.token.cancel();
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            self.remaining -= 1;
            let count = output.len().min(self.input.len()).min(3);
            output[..count].copy_from_slice(&self.input[..count]);
            self.input = &self.input[count..];
            Ok(count)
        }
    }
    struct Sink<'a>(&'a CancellationToken);
    impl Write for Sink<'_> {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            self.0.cancel();
            Err(std::io::ErrorKind::Interrupted.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let roles = catalog();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let captured = roles
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let reader_token = CancellationToken::new();
    let mut reader = captured.image_reader(&reader_token).unwrap();
    let mut sink = Sink(&reader_token);
    assert_eq!(
        reader.write_to(&mut sink).unwrap_err().sqlstate(),
        Some("57014")
    );
    assert_eq!(
        reader.read(&mut [0; 1]).unwrap_err().sqlstate(),
        Some("XX001")
    );
    let bytes = encode_roles(&roles);
    for remaining in [0, 8, 24, 50] {
        let token = CancellationToken::new();
        let memory = MemoryBudget::new(1 << 20);
        let mut source = Source {
            input: &bytes,
            token: &token,
            remaining,
        };
        let result = RoleDefinitions::read_image(&mut source, &memory, &token);
        assert!(result.is_err());
        assert_eq!(result.err().unwrap().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn catalog_reader_retires_on_writer_unwind_and_keeps_completed_eof() {
    struct Panic;
    impl Write for Panic {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            panic!("injected image writer unwind")
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let roles = catalog();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let captured = roles
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let mut reader = captured.image_reader(&token).unwrap();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.write_to(&mut Panic)))
            .is_err()
    );
    assert_eq!(
        reader.read(&mut [0; 8]).unwrap_err().sqlstate(),
        Some("XX001")
    );
    let mut reader = captured.image_reader(&token).unwrap();
    let mut bytes = [0; 4096];
    while reader.read(&mut bytes).unwrap() != 0 {}
    token.cancel();
    assert_eq!(reader.read(&mut bytes).unwrap(), 0);
}

#[test]
fn schema_and_database_import_require_and_retain_the_complete_allowance() {
    let token = CancellationToken::new();
    let capture_memory = MemoryBudget::new(1 << 20);
    let control = ProductionControl::new(&capture_memory, &token, &token);
    let schemas = schema_registry().capture(&control).unwrap();
    let source = DatabaseAuthority::with_budget(
        &BoundDatabaseSecurity {
            role_owner: RoleIdentity::BOOTSTRAP,
            acl: Some(vec![
                BoundAclEntry {
                    role: None,
                    grantor: RoleIdentity::BOOTSTRAP,
                    privileges: DatabasePrivileges::ALL,
                    grant_options: DatabasePrivileges::default()
                };
                64
            ]),
        },
        &MemoryBudget::new(1 << 20),
    )
    .unwrap();
    let database = source.capture(&control).unwrap();
    let schema_bytes = streamed(schemas.image_reader(&token).unwrap(), 13);
    let database_bytes = streamed(database.image_reader(&token).unwrap(), 13);
    for database in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        if database {
            drop(
                DatabaseAuthority::read_image(&mut database_bytes.as_slice(), &memory, &token)
                    .unwrap(),
            );
        } else {
            drop(
                SchemaRegistry::read_image(&mut schema_bytes.as_slice(), &memory, &token).unwrap(),
            );
        }
        let required = memory.peak();
        assert!(required > 0);
        assert_eq!(memory.used(), 0);
        for limit in [0, required - 1] {
            let memory = MemoryBudget::new(limit);
            let error = if database {
                DatabaseAuthority::read_image(&mut database_bytes.as_slice(), &memory, &token)
                    .err()
                    .unwrap()
            } else {
                SchemaRegistry::read_image(&mut schema_bytes.as_slice(), &memory, &token)
                    .err()
                    .unwrap()
            };
            assert_eq!(error.sqlstate(), Some("53200"));
            assert_eq!(memory.used(), 0);
        }
        let memory = MemoryBudget::new(required);
        if database {
            let restored =
                DatabaseAuthority::read_image(&mut database_bytes.as_slice(), &memory, &token)
                    .unwrap();
            let clone = restored.clone();
            drop(restored);
            assert!(memory.used() > 0);
            let captured = clone.capture(&control).unwrap();
            drop(clone);
            assert!(memory.used() > 0);
            assert_eq!(
                streamed(captured.image_reader(&token).unwrap(), 13),
                database_bytes
            );
            drop(captured);
        } else {
            let restored =
                SchemaRegistry::read_image(&mut schema_bytes.as_slice(), &memory, &token).unwrap();
            let clone = restored.clone();
            drop(restored);
            assert!(memory.used() > 0);
            let captured = clone.capture(&control).unwrap();
            drop(clone);
            assert!(memory.used() > 0);
            assert_eq!(
                streamed(captured.image_reader(&token).unwrap(), 13),
                schema_bytes
            );
            drop(captured);
        }
        assert_eq!(memory.used(), 0);
    }
}
