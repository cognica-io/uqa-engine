//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::shell_type::StoredShellType;
use super::*;
use uqa_core::{
    catalog_role::RoleIdentity, memory::MemoryBudget, CancellationToken, RelationIdentity,
};

fn shell(schema: &str, name: &str) -> StoredShellType {
    StoredShellType {
        object_id: [1; 16],
        oid: 20_000,
        identity: RelationIdentity::new(schema, name),
        owner: RoleIdentity::BOOTSTRAP,
        usage_acl: None,
    }
}

#[test]
fn type_roots_capture_without_visiting_definitions_or_temporary_payloads() {
    for count in [1, 64, 4096] {
        let mut source = TypeDefinitions::new();
        for index in 0..count {
            let name = format!("type_{index:04}");
            source
                .insert(format!("public.{name}"), shell("public", &name))
                .unwrap();
        }
        let budget = MemoryBudget::new(16 << 20);
        let cancel = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancel, &cancel);
        let mut captured = None;
        let allocations =
            allocation_counter::measure(|| captured = Some(source.capture(&control).unwrap()));
        assert_eq!(allocations.count_total, 0);
        let bytes = budget.used();
        let selected = captured.take().unwrap();
        assert_eq!(selected.len(), count);
        let expected = serde_json::to_value(&*selected).unwrap();
        let large = "temporary".repeat(1 << 18);
        source
            .insert(format!("pg_temp_7.{large}"), shell("pg_temp_7", &large))
            .unwrap();
        source
            .insert(
                "pg_toast_temp_7.hidden".into(),
                shell("pg_toast_temp_7", "hidden"),
            )
            .unwrap();
        let second = MemoryBudget::new(bytes);
        let second_control = ProductionControl::new(&second, &cancel, &cancel);
        let allocations = allocation_counter::measure(|| {
            captured = Some(source.capture(&second_control).unwrap());
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(second.used(), bytes);
        assert_eq!(
            serde_json::to_value(&**captured.as_ref().unwrap()).unwrap(),
            expected
        );
        source
            .insert("public.later".into(), shell("public", "later"))
            .unwrap();
        source.remove("public.type_0000").unwrap();
        drop(source);
        assert_eq!(serde_json::to_value(&*selected).unwrap(), expected);
        assert_eq!(
            serde_json::to_value(&**captured.as_ref().unwrap()).unwrap(),
            expected
        );
        drop((selected, captured));
        assert_eq!(budget.used(), 0);
        assert_eq!(second.used(), 0);
    }
}

#[test]
fn native_type_views_merge_canonical_partitions_and_preserve_wire_encodings() {
    let expected = std::collections::BTreeMap::from([
        ("z.last".to_owned(), shell("z", "last")),
        ("a.first".to_owned(), shell("a", "first")),
        (
            "pg_temp_4.private".to_owned(),
            shell("pg_temp_4", "private"),
        ),
    ]);
    let source = TypeDefinitions::try_from_iter(expected.clone()).unwrap();
    assert_eq!(
        source.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        serde_json::to_value(&source).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    let mut restored: TypeDefinitions<StoredShellType> =
        serde_json::from_value(serde_json::to_value(&source).unwrap()).unwrap();
    let memory = MemoryBudget::new(32768);
    let cancel = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancel, &cancel);
    let legacy = super::super::definition_capture::types::shells(&expected, &control).unwrap();
    let captured = restored.capture(&control).unwrap();
    assert_eq!(
        serde_json::to_value(&*legacy).unwrap(),
        serde_json::to_value(&*captured).unwrap()
    );
    restored
        .edit("a.first", |value| {
            value.oid = 22_000;
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert_eq!(source["a.first"].oid, 20_000);
    assert_eq!(captured.get("a.first").unwrap().oid, 20_000);
    assert_eq!(restored["a.first"].oid, 22_000);
    restored.clear();
    assert!(restored.is_empty());
    assert_eq!(captured.len(), 2);
}

#[test]
fn failed_type_publication_and_independent_capture_leave_original_roots_usable() {
    let native = MemoryBudget::new(32768);
    let mut source = TypeDefinitions::with_budget(&native);
    for name in ["first", "middle", "last"] {
        source
            .insert(format!("public.{name}"), shell("public", name))
            .unwrap();
    }
    let before = serde_json::to_value(&source).unwrap();
    let original = source.clone();
    let blocked = native.reserve(native.available()).unwrap();
    assert!(source
        .insert("public.middle".into(), shell("public", "middle"))
        .is_err());
    assert!(source.remove("public.middle").is_err());
    drop(blocked);
    assert_eq!(serde_json::to_value(&source).unwrap(), before);
    assert_eq!(serde_json::to_value(&original).unwrap(), before);
    let capture = MemoryBudget::new(32768);
    let cancel = CancellationToken::new();
    let control = ProductionControl::new(&capture, &cancel, &cancel);
    let selected = source.capture(&control).unwrap();
    let required = capture.used();
    let insufficient = MemoryBudget::new(required - 1);
    assert!(source
        .capture(&ProductionControl::new(&insufficient, &cancel, &cancel))
        .is_err());
    assert_eq!(insufficient.used(), 0);
    let exact = MemoryBudget::new(required);
    let second = source
        .capture(&ProductionControl::new(&exact, &cancel, &cancel))
        .unwrap();
    cancel.cancel();
    assert!(matches!(
        source.capture(&control),
        Err(SQLError::Cancelled(_))
    ));
    drop((source, original));
    assert!(native.used() > 0);
    drop(selected);
    assert_eq!(capture.used(), 0);
    assert_eq!(exact.used(), required);
    drop(second);
    assert_eq!(exact.used(), 0);
    assert_eq!(native.used(), 0);
}

#[test]
fn domain_root_publication_shares_unrelated_payloads_and_bounds_selected_path_work() {
    use super::super::domain::StoredDomain;
    fn domain(name: &str, payload: &str) -> StoredDomain {
        let crate::Statement::CreateDomain(definition) = crate::compile(&format!(
            "CREATE DOMAIN public.{name} AS text DEFAULT '{payload}'"
        ))
        .unwrap()
        .remove(0) else {
            panic!("domain")
        };
        StoredDomain {
            object_id: [1; 16],
            oid: 20_000,
            array_oid: Some(20_001),
            identity: RelationIdentity::new("public", name),
            owner: RoleIdentity::BOOTSTRAP,
            definition,
            array_name: Some(format!("_{name}")),
            usage_acl: None,
        }
    }
    for count in [1, 32, 512] {
        let mut source = TypeDefinitions::new();
        let payload = "x".repeat(16384);
        for index in 0..count {
            let name = format!("domain_{index:04}");
            source
                .insert(format!("public.{name}"), domain(&name, &payload))
                .unwrap();
        }
        let original = source.clone();
        let replacement = domain("selected", "small");
        let allocations = allocation_counter::measure(|| {
            source
                .insert("public.selected".into(), replacement)
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
        for name in original.keys() {
            assert!(source.shares_definition(name, &original));
        }
        let memory = MemoryBudget::new(32 << 20);
        let cancel = CancellationToken::new();
        let control = ProductionControl::new(&memory, &cancel, &cancel);
        let mut captured = None;
        let allocations =
            allocation_counter::measure(|| captured = Some(source.capture(&control).unwrap()));
        assert_eq!(allocations.count_total, 0);
        assert!(memory.used() >= count * payload.len());
        source
            .edit("public.domain_0000", |value| {
                value.definition.default = Some(crate::ast::Expr::Literal(uqa_core::Value::Str(
                    "changed".into(),
                )));
                Ok(())
            })
            .unwrap()
            .unwrap();
        let captured = captured.unwrap();
        assert_eq!(
            serde_json::to_value(captured.get("public.domain_0000").unwrap()).unwrap(),
            serde_json::to_value(&original["public.domain_0000"]).unwrap()
        );
        drop((original, source, captured));
        assert_eq!(memory.used(), 0);
    }
}
