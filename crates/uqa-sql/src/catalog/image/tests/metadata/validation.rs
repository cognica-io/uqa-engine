//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn rejects<T: ImageDefinition>(bytes: &[u8]) {
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    assert!(T::decode(&mut &*bytes, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}
fn malformed<T: ImageDefinition>(values: &NamedDefinitions<T>) {
    let bytes = encode(values);
    for cut in 0..bytes.len() {
        rejects::<T>(&bytes[..cut]);
    }
    let mut corrupt = bytes.clone();
    corrupt.push(0);
    rejects::<T>(&corrupt);
    corrupt = bytes.clone();
    corrupt[0] ^= 1;
    rejects::<T>(&corrupt);
    corrupt = bytes.clone();
    corrupt[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    rejects::<T>(&corrupt);
    corrupt = bytes.clone();
    corrupt[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    rejects::<T>(&corrupt);
    corrupt = bytes.clone();
    corrupt[24] = 255;
    rejects::<T>(&corrupt);
    corrupt = bytes.clone();
    corrupt[8..16].copy_from_slice(&2_u64.to_le_bytes());
    corrupt.extend_from_slice(&bytes[16..]);
    rejects::<T>(&corrupt);
}

#[test]
fn metadata_images_reject_all_truncations_trailing_bytes_bad_utf8_and_duplicate_keys() {
    malformed(&singleton("public.e", enum_value()));
    malformed(&singleton("public.s", shell_value()));
    malformed(&singleton("s", server_value()));
    malformed(&singleton("g", graph_value()));
}

#[test]
fn enum_images_reject_aliasing_names_identities_labels_and_invalid_order() {
    for corrupt in 0..11 {
        let mut value = enum_value();
        match corrupt {
            0 => value.object_id = [0; 16],
            1 => value.oid = 1,
            2 => value.array_oid = value.oid,
            3 => value.array_name.clear(),
            4 => value.identity.name = "different".into(),
            5 => value.owner.object_id = [0; 16],
            6 => value.labels[1].oid = value.labels[0].oid,
            7 => value.labels[1].key = value.labels[0].key.clone(),
            8 => value.labels[1].sort_order = f32::NAN,
            9 => value.labels[1].sort_order = value.labels[0].sort_order,
            _ => value.labels[1].label = value.labels[0].label.clone(),
        }
        rejects::<StoredEnum>(&encode(&singleton("public.e", value)));
    }
    let mut values = singleton("public.e", enum_value());
    let mut other = enum_value();
    other.identity.name = "z".into();
    values.insert("public.z".into(), other).unwrap();
    rejects::<StoredEnum>(&encode(&values));
    let mut value = enum_value();
    value.labels[0].label = "é".repeat(32);
    rejects::<StoredEnum>(&encode(&singleton("public.e", value)));
    let mut bytes = encode(&singleton("public.e", enum_value()));
    // The first immutable key follows the 122-byte record prefix and 12-byte label prefix.
    assert_eq!(bytes[134], 64);
    bytes[134] = 0;
    rejects::<StoredEnum>(&bytes);
}

#[test]
fn shell_images_reject_invalid_names_oids_incarnations_and_boolean_tags() {
    for corrupt in 0..5 {
        let mut value = shell_value();
        match corrupt {
            0 => value.object_id = [0; 16],
            1 => value.oid = 0,
            2 => value.identity.schema.clear(),
            3 => value.identity.name = "wrong".into(),
            _ => value.owner.oid = 0,
        }
        rejects::<StoredShellType>(&encode(&singleton("public.s", value)));
    }
    let mut bytes = encode(&singleton("public.s", shell_value()));
    let flag = bytes.len() - 9;
    assert_eq!(bytes[flag], 1);
    bytes[flag] = 2;
    rejects::<StoredShellType>(&bytes);
    let mut values = singleton("public.s", shell_value());
    let mut other = shell_value();
    other.identity.name = "z".into();
    values.insert("public.z".into(), other).unwrap();
    rejects::<StoredShellType>(&encode(&values));
}

#[test]
fn foreign_server_images_reject_metadata_aliases_and_inconsistent_option_order() {
    for corrupt in 0..10 {
        let mut value = server_value();
        match corrupt {
            0 => value.name = "wrong".into(),
            1 => value.fdw_type.clear(),
            2 => value.metadata.oid = 1,
            3 => value.metadata.object_id = [0; 16],
            4 => value.metadata.owner.object_id = [0; 16],
            5 => value.metadata.option_order = Some(vec![]),
            6 => value.metadata.option_order = Some(vec!["a".into(), "a".into()]),
            7 => value.metadata.option_order = Some(vec!["unknown".into(), "a".into()]),
            8 => value.metadata.wrapper_reference.as_mut().unwrap().oid = 0,
            _ => value.metadata.wrapper_reference.as_mut().unwrap().object_id = [0; 16],
        }
        rejects::<ForeignServerDefinition>(&encode(&singleton("s", value)));
    }
    let mut values = singleton("s", server_value());
    let mut other = server_value();
    other.name = "z".into();
    values.insert("z".into(), other).unwrap();
    rejects::<ForeignServerDefinition>(&encode(&values));
}

#[test]
fn graph_oid_images_reject_duplicate_claims_zero_addresses_and_label_keys() {
    for corrupt in 0..7 {
        let mut value = graph_value();
        let label = value.labels.get_mut(&17).unwrap();
        match corrupt {
            0 => value.namespace = 0,
            1 => value.label_sequence = value.namespace,
            2 => label.sequence = value.namespace,
            3 => label.relation.row_type = Some(0),
            4 => label
                .not_null
                .insert("invalid".into(), 0)
                .map_or((), |_| ()),
            5 => label
                .not_null
                .insert("id2".into(), label.not_null["id"])
                .map_or((), |_| ()),
            _ => {
                let label = label.clone();
                value.labels.clear();
                value.labels.insert(0, label);
            }
        }
        rejects::<GraphCatalogOids>(&encode(&singleton("g", value)));
    }
    let mut values = singleton("a", graph_value());
    values.insert("z".into(), graph_value()).unwrap();
    rejects::<GraphCatalogOids>(&encode(&values));
}

#[test]
fn metadata_images_preserve_empty_roots_and_reject_reversed_child_maps() {
    assert_round_trip(
        &NamedDefinitions::<StoredEnum>::new(),
        b"UQAENU01\0\0\0\0\0\0\0\0",
    );
    assert_round_trip(
        &NamedDefinitions::<StoredShellType>::new(),
        b"UQASHL01\0\0\0\0\0\0\0\0",
    );
    assert_round_trip(
        &NamedDefinitions::<ForeignServerDefinition>::new(),
        b"UQAFSR01\0\0\0\0\0\0\0\0",
    );
    assert_round_trip(
        &NamedDefinitions::<GraphCatalogOids>::new(),
        b"UQAGOI01\0\0\0\0\0\0\0\0",
    );
    let mut servers = encode(&singleton("s", server_value()));
    let at = servers
        .windows(10)
        .position(|part| part == b"\x01\0\0\0\0\0\0\0a\x02")
        .unwrap();
    servers[at + 8] = b'z';
    rejects::<ForeignServerDefinition>(&servers);
    let mut graphs = encode(&singleton("g", graph_value()));
    let at = graphs
        .windows(10)
        .position(|part| part == b"\x02\0\0\0\0\0\0\0id")
        .unwrap();
    graphs[at + 8..at + 10].copy_from_slice(b"zz");
    rejects::<GraphCatalogOids>(&graphs);
}
