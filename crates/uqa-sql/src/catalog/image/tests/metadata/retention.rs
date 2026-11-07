//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

pub(super) fn retained<T: ImageDefinition>(values: &NamedDefinitions<T>) {
    let bytes = encode(values);
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(8 << 20);
    let mut decoded = None;
    let allocations = allocation_counter::measure(|| {
        decoded = Some(T::decode(&mut bytes.as_slice(), &memory, &token).unwrap());
    });
    let retained = memory.used();
    let peak = memory.peak();
    // The native allowance excludes the strong/weak counters of each shared node and entry; the allocator probe includes them.
    let reference_counts = 4 * std::mem::size_of::<usize>() * values.len();
    assert_eq!(
        allocations.bytes_current,
        (retained + reference_counts) as i64
    );
    drop(decoded);
    assert_eq!(memory.used(), 0);
    let exact = MemoryBudget::new(peak);
    let mut decoded = T::decode(&mut bytes.as_slice(), &exact, &token).unwrap();
    assert_eq!(exact.used(), retained);
    let shared = decoded.clone();
    let saved = capture(&decoded, &memory, &token);
    assert_eq!(memory.used(), retained);
    decoded.clear();
    drop((decoded, shared));
    assert_eq!(exact.used(), retained);
    assert_eq!(streamed(T::reader(&saved, &token), 7), bytes);
    drop(saved);
    assert_eq!(exact.used(), 0);
    assert_eq!(memory.used(), 0);
    for limit in [0, 1, 64, retained - 1] {
        let budget = MemoryBudget::new(limit);
        let result = T::decode(&mut bytes.as_slice(), &budget, &token);
        assert!(result.is_err());
        assert_eq!(result.err().unwrap().sqlstate(), Some("53200"));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn metadata_import_admits_exact_native_payload_and_retains_shared_generations() {
    retained(&singleton("public.e", enum_value()));
    retained(&singleton("public.s", shell_value()));
    retained(&singleton("s", server_value()));
    retained(&singleton("g", graph_value()));
}

pub(super) fn cancellation<T: ImageDefinition>(values: &NamedDefinitions<T>) {
    struct Source<'a> {
        bytes: &'a [u8],
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
            let count = output
                .len()
                .min(3)
                .min(self.remaining)
                .min(self.bytes.len());
            output[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes = &self.bytes[count..];
            self.remaining -= count;
            Ok(count)
        }
    }
    let bytes = encode(values);
    for remaining in [0, bytes.len() / 2, bytes.len() - 1] {
        let token = CancellationToken::new();
        let memory = MemoryBudget::new(8 << 20);
        let mut source = Source {
            bytes: &bytes,
            token: &token,
            remaining,
        };
        let result = T::decode(&mut source, &memory, &token);
        assert!(result.is_err());
        assert_eq!(result.err().unwrap().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn cancelled_metadata_import_retires_partial_vectors_maps_and_validation_state() {
    cancellation(&singleton("public.e", enum_value()));
    cancellation(&singleton("public.s", shell_value()));
    cancellation(&singleton("s", server_value()));
    cancellation(&singleton("g", graph_value()));
}

pub(super) fn bounded<T: ImageDefinition>(values: &NamedDefinitions<T>) {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let saved = capture(values, &memory, &token);
    let expected = encode(values);
    let mut output = vec![0; 20000];
    let allocations = allocation_counter::measure(|| {
        let mut reader = T::reader(&saved, &token);
        let mut position = 0;
        loop {
            let count = reader.read(&mut output).unwrap();
            assert!(count <= 16384);
            assert_eq!(&output[..count], &expected[position..position + count]);
            position += count;
            if count == 0 {
                break;
            }
        }
        assert_eq!(position, expected.len());
    });
    assert_eq!(allocations.count_total, 0);
    let token = CancellationToken::new();
    let mut reader = T::reader(&saved, &token);
    assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
    token.cancel();
    assert_eq!(
        reader.read(&mut output).unwrap_err().sqlstate(),
        Some("57014")
    );
    assert_eq!(
        reader.read(&mut output).unwrap_err().sqlstate(),
        Some("XX001")
    );
}

#[test]
fn metadata_readers_stream_large_native_children_without_allocation_or_preparation_scan() {
    let mut value = enum_value();
    value.labels = EnumLabelKey::initial(4096)
        .unwrap()
        .into_iter()
        .enumerate()
        .map(|(index, key)| StoredEnumLabel {
            oid: 70000 + index as u32,
            key,
            label: format!("label_{index}"),
            sort_order: index as f32,
        })
        .collect();
    bounded(&singleton("public.e", value));
    let mut shell = shell_value();
    shell.usage_acl = Some(acl().into_iter().cycle().take(4096).collect());
    bounded(&singleton("public.s", shell));
    let mut server = server_value();
    server
        .options
        .insert("a".into(), "original α payload".repeat(65536));
    bounded(&singleton("s", server));
    let mut graph = graph_value();
    let label = graph.labels.get(&17).unwrap().clone();
    for key in 18..4114 {
        graph.labels.insert(key, label.clone());
    }
    bounded(&singleton("g", graph));
}

#[test]
fn type_images_exclude_native_temporary_roots_and_keep_quoted_canonical_keys() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut enums = singleton("public.e", enum_value());
    let mut shells = singleton("public.s", shell_value());
    let mut temporary_enum = enum_value();
    temporary_enum.identity.schema = "pg_temp_123".into();
    let mut temporary_shell = shell_value();
    temporary_shell.identity.schema = "pg_temp_123".into();
    enums
        .insert(temporary_enum.identity.qualified_name(), temporary_enum)
        .unwrap();
    shells
        .insert(temporary_shell.identity.qualified_name(), temporary_shell)
        .unwrap();
    let saved_enums = capture(&enums, &memory, &token);
    let saved_shells = capture(&shells, &memory, &token);
    assert_eq!(saved_enums.len(), 1);
    assert_eq!(saved_shells.len(), 1);
    let mut value = enum_value();
    value.identity = RelationIdentity::new("Quoted.Schema", "a\"b");
    let key = value.identity.qualified_name();
    let enums = singleton(&key, value);
    let bytes = encode(&enums);
    let restored = StoredEnum::decode(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert_eq!(
        restored[&key].identity,
        RelationIdentity::new("Quoted.Schema", "a\"b")
    );
}
