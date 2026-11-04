//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::FunctionBinding,
    catalog::{foreign_wrapper::*, roles::RoleIdentity},
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn callback() -> ForeignWrapperFunction {
    ForeignWrapperFunction {
        oid: 24001,
        binding: FunctionBinding {
            object_id: Some([9; 16]),
            name: "\"Quoted.Schema\".callback".into(),
            argument_types: vec!["text[]".into(), "oid".into()],
            builtin: false,
            dispatch: None,
            invocation: None,
            value_binding: None,
            resolution_error: None,
            input_collation: Some(
                crate::catalog::collations::CollationSelection::from_oid(100).unwrap(),
            ),
        },
    }
}

fn server() -> ForeignServerDefinition {
    ForeignServerDefinition {
        name: "Exact.Server".into(),
        fdw_type: "Exact.Wrapper".into(),
        options: BTreeMap::from([
            ("z".into(), "first\nvalue".into()),
            ("a".into(), "second 한글".into()),
        ]),
        metadata: ForeignServerMetadata {
            oid: 24003,
            object_id: [3; 16],
            owner: RoleIdentity::BOOTSTRAP,
            server_type: Some("Original.Type".into()),
            version: Some("".into()),
            option_order: Some(vec!["z".into(), "a".into()]),
            wrapper_reference: Some(ForeignWrapperReference {
                oid: 24002,
                object_id: [2; 16],
            }),
        },
    }
}

#[test]
fn foreign_wrapper_capture_preserves_every_handler_and_bound_callback_without_resolution() {
    let mut source = native_wrappers();
    for (name, handler) in [
        ("no_handler", ForeignWrapperHandler::None),
        ("callback", ForeignWrapperHandler::Function(callback())),
    ] {
        source
            .insert(
                name.into(),
                ForeignWrapperDefinition {
                    name: name.into(),
                    identity: ForeignWrapperReference {
                        oid: 24002,
                        object_id: [2; 16],
                    },
                    owner: RoleIdentity::BOOTSTRAP,
                    handler,
                    validator: Some(callback()),
                    options: vec![("z".into(), "first".into()), ("a".into(), "second".into())],
                },
            )
            .unwrap();
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut saved = None;
    let actual = allocation_counter::measure(|| saved = Some(wrappers(&source, &control).unwrap()));
    let saved = saved.unwrap();
    assert!(saved.iter().map(|(k, v)| (k, v)).eq(source.iter()));
    assert_eq!(actual.bytes_current, i64::try_from(memory.used()).unwrap());
    drop(source);
    assert_eq!(saved.len(), 5);
    drop(saved);
    assert_eq!(memory.used(), 0);
}

#[test]
fn foreign_server_capture_keeps_option_order_legacy_absence_and_original_references() {
    let original = server();
    let mut legacy = server();
    legacy.metadata.option_order = None;
    legacy.metadata.wrapper_reference = None;
    legacy.metadata.server_type = None;
    legacy.metadata.version = None;
    let source = BTreeMap::from([("current".into(), original), ("legacy".into(), legacy)]);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut saved = None;
    let actual = allocation_counter::measure(|| saved = Some(servers(&source, &control).unwrap()));
    let saved = saved.unwrap();
    assert_eq!(saved.len(), source.len());
    for ((key, captured), (original_key, original)) in saved.iter().zip(&source) {
        assert_eq!(key, original_key);
        assert_eq!(captured.name, original.name);
        assert_eq!(captured.fdw_type, original.fdw_type);
        assert_eq!(captured.metadata, original.metadata);
        assert!(captured
            .options
            .iter()
            .map(|(k, v)| (k, v))
            .eq(original.options.iter()));
    }
    assert_eq!(actual.bytes_current, i64::try_from(memory.used()).unwrap());
    drop(source);
    drop(saved);
    assert_eq!(memory.used(), 0);
}

#[test]
fn foreign_descriptor_capture_rejects_unfunded_options_and_original_cancellation() {
    let mut wrapper_source = native_wrappers();
    wrapper_source
        .edit("memory_fdw", |wrapper| {
            wrapper.options.push(("large".into(), "x".repeat(32768)));
            Ok(())
        })
        .unwrap()
        .unwrap();
    let mut value = server();
    value.options.insert("large".into(), "x".repeat(32768));
    let server_source = BTreeMap::from([("source".into(), value)]);
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    assert_eq!(
        wrappers(&wrapper_source, &control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    assert_eq!(
        servers(&server_source, &control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    token.cancel();
    assert!(matches!(
        wrappers(&wrapper_source, &control),
        Err(SQLError::Cancelled(_))
    ));
    assert!(matches!(
        servers(&server_source, &control),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}
