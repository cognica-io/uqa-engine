//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    definition_capture::foreign, foreign_server::ForeignServerMetadata,
    foreign_wrapper::ForeignWrapperReference, roles::RoleIdentity,
};
use uqa_core::{
    memory::{MemoryBudget, OwnedMap},
    CancellationToken,
};

fn server(index: u32) -> ForeignServerDefinition {
    ForeignServerDefinition {
        name: format!("server_{index:04}"),
        fdw_type: "Exact.Wrapper".into(),
        options: OwnedMap::from_iter([
            ("z".into(), "first\nvalue".into()),
            ("a".into(), "second 한글".into()),
        ]),
        metadata: ForeignServerMetadata {
            oid: 20000 + index,
            object_id: u128::from(index + 1).to_be_bytes(),
            owner: RoleIdentity::BOOTSTRAP,
            server_type: (!index.is_multiple_of(3)).then(|| "Original.Type".into()),
            version: (index % 3 != 1).then(String::new),
            option_order: (index % 3 != 2).then(|| vec!["z".into(), "a".into()]),
            wrapper_reference: (index % 3 != 2).then_some(ForeignWrapperReference {
                oid: 40000,
                object_id: [9; 16],
            }),
        },
    }
}

#[test]
fn server_roots_admit_exact_option_storage_and_capture_original_values_without_allocations() {
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(64 << 20);
    let control = ProductionControl::new(&budget, &token, &token);
    let value = server(1);
    let mut copied = None;
    let allocations =
        allocation_counter::measure(|| copied = Some(value.copy_definition(&control).unwrap()));
    let bytes = budget.used();
    assert_eq!(allocations.bytes_current, i64::try_from(bytes).unwrap());
    assert_eq!(&**copied.as_ref().unwrap(), &value);
    drop(copied);
    assert_eq!(budget.used(), 0);
    let small = MemoryBudget::new(bytes - 1);
    assert!(value
        .copy_definition(&ProductionControl::new(&small, &token, &token))
        .is_err());
    assert_eq!(small.used(), 0);
    for count in [1, 64, 4096] {
        let mut servers = ForeignServers::try_from_iter((0..count).map(|index| {
            let value = server(index);
            (value.name.clone(), value)
        }))
        .unwrap();
        for (index, (_, value)) in servers.iter().enumerate() {
            assert_eq!(value, &server(u32::try_from(index).unwrap()));
        }
        let original = servers.clone();
        let mut saved = None;
        let allocations =
            allocation_counter::measure(|| saved = Some(servers.capture(&control).unwrap()));
        assert_eq!(allocations.count_total, 0);
        let exact = MemoryBudget::new(budget.used());
        let independent = servers
            .capture(&ProductionControl::new(&exact, &token, &token))
            .unwrap();
        assert_eq!(exact.used(), budget.used());
        let legacy = foreign::servers(&servers, &control).unwrap();
        for ((name, value), (old_name, old)) in saved.as_ref().unwrap().iter().zip(legacy.iter()) {
            assert_eq!(name, old_name);
            assert_eq!(value.name, old.name);
            assert_eq!(value.fdw_type, old.fdw_type);
            assert_eq!(value.metadata, old.metadata);
            assert!(value
                .options
                .iter()
                .eq(old.options.iter().map(|(k, v)| (k, v))));
            let map = value
                .options
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<std::collections::BTreeMap<_, _>>();
            let encoded = serde_json::to_vec(&map).unwrap();
            assert_eq!(serde_json::to_vec(&value.options).unwrap(), encoded);
            assert_eq!(
                serde_json::from_slice::<OwnedMap<String, String>>(&encoded).unwrap(),
                value.options
            );
        }
        servers.clear();
        drop((servers, legacy));
        assert!(saved.as_ref().unwrap().iter().eq(original.iter()));
        assert!(independent.iter().eq(original.iter()));
        drop((saved, independent));
        assert_eq!(budget.used(), 0);
        assert_eq!(exact.used(), 0);
    }
}

#[test]
fn selected_server_publication_shares_unrelated_options_and_metadata() {
    for count in [1, 32, 512] {
        let mut servers = ForeignServers::try_from_iter((0..count).map(|index| {
            let value = server(index);
            (value.name.clone(), value)
        }))
        .unwrap();
        let mut large = server(9000);
        large
            .options
            .insert("large".into(), "value".repeat(1 << 15));
        large.metadata.server_type = Some("type".repeat(1 << 15));
        servers.insert(large.name.clone(), large).unwrap();
        let original = servers.clone();
        let allocations = allocation_counter::measure(|| {
            servers
                .edit("server_0000", |value| {
                    value.metadata.version = Some("changed".into());
                    value.options.insert("z".into(), "replacement".into());
                    Ok(())
                })
                .unwrap()
                .unwrap();
        });
        assert!(
            allocations.bytes_total < 32768,
            "count={count}: {allocations:?}"
        );
        assert!(
            allocations.count_total < 256,
            "count={count}: {allocations:?}"
        );
        for name in original.keys().filter(|name| *name != "server_0000") {
            assert!(servers.shares_definition(name, &original));
        }
        assert_eq!(original["server_0000"], server(0));
        assert_eq!(servers["server_0000"].options["z"], "replacement");
        assert_eq!(
            servers["server_0000"].metadata.version.as_deref(),
            Some("changed")
        );
    }
}

#[test]
fn denied_server_publication_and_capture_preserve_original_references_and_release_leases() {
    let native = MemoryBudget::new(65536);
    let mut servers = ForeignServers::with_budget(&native);
    servers.insert("server_0000".into(), server(0)).unwrap();
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let saved = servers.capture(&control).unwrap();
    let small = MemoryBudget::new(budget.used() - 1);
    assert!(servers
        .capture(&ProductionControl::new(&small, &token, &token))
        .is_err());
    assert_eq!(small.used(), 0);
    let blocked = native.reserve(native.available()).unwrap();
    assert!(servers
        .edit("server_0000", |value| {
            value.metadata.wrapper_reference = None;
            Ok(())
        })
        .is_err());
    assert_eq!(servers["server_0000"], server(0));
    assert_eq!(saved.get("server_0000"), Some(&server(0)));
    token.cancel();
    assert!(matches!(
        servers.capture(&control),
        Err(SQLError::Cancelled(_))
    ));
    drop((blocked, servers));
    assert!(native.used() > 0);
    drop(saved);
    assert_eq!(native.used(), 0);
    assert_eq!(budget.used(), 0);
}
