//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::Statement,
    catalog::events::{
        RuleColumnDependency, RuleConditionBinding, RuleDependencies, RuleRoutineDependency,
    },
    plan::ExpressionPlan,
    ScalarExpr,
};
use uqa_core::memory::ProductionControl;

mod resources;

fn fixture() -> RuleCatalog {
    let Statement::CreateRule(definition) = crate::compile("CREATE RULE watch AS ON UPDATE TO public.items WHERE NEW.id > OLD.id DO ALSO INSERT INTO public.audit SELECT NEW.id").unwrap().remove(0) else { panic!("rule") };
    let binding =
        RuleConditionBinding::for_event(&["id".into(), "Removed.Name".into()], RuleEvent::Update);
    let value = StoredRule {
        definition,
        enabled: EventEnableMode::Replica,
        condition_plan: Some(ExpressionPlan {
            scalar: ScalarExpr::InternalColumn(binding.old_column("Removed.Name").unwrap()),
            subqueries: Vec::new(),
        }),
        condition_binding: Some(binding),
        dependencies: Some(RuleDependencies {
            relations: [
                RelationIdentity::new("public", "audit"),
                RelationIdentity::new("public", "items"),
            ]
            .into(),
            columns: [RuleColumnDependency {
                relation: RelationIdentity::new("public", "items"),
                column: "id".into(),
            }]
            .into(),
            routines: [RuleRoutineDependency {
                object_id: Some([4; 16]),
                name: "public.f".into(),
                argument_types: vec!["integer".into(), "text".into()],
            }]
            .into(),
        }),
        catalog_oid: Some(40_001),
    };
    let mut catalog = RuleCatalog::new();
    catalog
        .insert_event(
            RelationIdentity::new("public", "items"),
            "watch".into(),
            value,
        )
        .unwrap();
    catalog
        .insert(
            RelationIdentity::new("public", "empty"),
            EventDefinitions::new(),
        )
        .unwrap();
    catalog
}

fn encode(source: &RuleCatalog) -> Vec<u8> {
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let captured = source
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let mut reader = captured.image_reader(&memory, &token).unwrap();
    drain(&mut reader)
}

fn drain(reader: &mut crate::catalog::image::DefinitionImageReader<'_>) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 29];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    bytes
}

#[test]
fn native_rule_images_preserve_original_actions_conditions_and_dependencies() {
    let source = fixture();
    let expected = serde_json::to_value(source.iter().collect::<Vec<_>>()).unwrap();
    let bytes = encode(&source);
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let restored = RuleCatalog::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert_eq!(
        serde_json::to_value(restored.iter().collect::<Vec<_>>()).unwrap(),
        expected
    );
    assert_eq!(encode(&restored), bytes);
    let independent = MemoryBudget::new(64 << 20);
    let captured = restored
        .capture(&ProductionControl::new(&independent, &token, &token))
        .unwrap();
    let entries = restored
        .get(&RelationIdentity::new("public", "items"))
        .unwrap()
        .clone();
    drop(restored);
    drop(source);
    assert_eq!(captured.len(), 2);
    drop(captured);
    assert_eq!(independent.used(), 0);
    assert!(memory.used() > 0);
    std::thread::spawn(move || {
        let value = entries.get("watch").unwrap();
        assert!(matches!(
            value.condition_plan.as_ref().unwrap().scalar,
            ScalarExpr::InternalColumn(_)
        ));
        assert_eq!(
            entries.iter().next().unwrap().0,
            entries.iter().next_back().unwrap().0
        );
        drop(entries);
    })
    .join()
    .unwrap();
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_rule_images_reject_every_partial_catalog_and_denied_admission() {
    let bytes = encode(&fixture());
    let token = CancellationToken::new();
    for end in 0..bytes.len() {
        let memory = MemoryBudget::new(64 << 20);
        assert!(
            RuleCatalog::read_image(&mut &bytes[..end], &memory, &token).is_err(),
            "prefix {end}"
        );
        assert_eq!(memory.used(), 0);
    }
    for limit in [0, 1, 128, 1024, 4096] {
        let memory = MemoryBudget::new(limit);
        assert!(RuleCatalog::read_image(&mut bytes.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
}

fn text(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend((value.len() as u64).to_le_bytes());
    bytes.extend(value.as_bytes());
}

fn independent(depth: usize) -> Vec<u8> {
    let mut bytes = b"UQARUL01".to_vec();
    bytes.extend(1u64.to_le_bytes());
    text(&mut bytes, "public");
    text(&mut bytes, "items");
    bytes.extend(1u64.to_le_bytes());
    text(&mut bytes, "watch");
    text(&mut bytes, "watch");
    text(&mut bytes, "public.items");
    bytes.extend([2, 0]); // UPDATE, ALSO.
    bytes.push(u8::from(depth != 0));
    if depth != 0 {
        bytes.extend(std::iter::repeat_n(14, depth));
        bytes.push(0);
    }
    bytes.extend(0u64.to_le_bytes()); // No actions.
    bytes.extend([0, 3]); // Replacement flag, ALWAYS.
    bytes.push(u8::from(depth != 0));
    if depth != 0 {
        bytes.extend(std::iter::repeat_n(14, depth));
        bytes.push(0);
        bytes.extend(0u64.to_le_bytes()); // Bound query arena.
        bytes.extend([1, 1]);
        bytes.extend(91u64.to_le_bytes());
        bytes.push(1);
        bytes.extend(92u64.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        text(&mut bytes, "id");
    } else {
        bytes.push(0);
    }
    bytes.extend([0, 0]); // Legacy absent dependencies and OID.
    bytes
}

#[test]
fn native_rule_images_match_independent_wire_and_reject_invalid_catalog_membership() {
    let bytes = independent(0);
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let restored = RuleCatalog::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert_eq!(encode(&restored), bytes);
    let value = restored
        .get(&RelationIdentity::new("public", "items"))
        .unwrap()
        .get("watch")
        .unwrap();
    assert_eq!(value.enabled, EventEnableMode::Always);
    assert!(value.definition.actions.is_empty());
    let mut wrong = RuleCatalog::new();
    wrong
        .insert_event(
            RelationIdentity::new("public", "wrong"),
            "watch".into(),
            value.clone(),
        )
        .unwrap();
    assert!(RuleCatalog::read_image(&mut encode(&wrong).as_slice(), &memory, &token).is_err());
    drop(restored);
    let mut duplicate = fixture();
    let relation = RelationIdentity::new("public", "items");
    let mut value = duplicate
        .get(&relation)
        .unwrap()
        .get("watch")
        .unwrap()
        .clone();
    value.definition.name = "watch_again".into();
    duplicate
        .insert_event(relation, "watch_again".into(), value)
        .unwrap();
    assert!(RuleCatalog::read_image(&mut encode(&duplicate).as_slice(), &memory, &token).is_err());
    for names in [["z", "a"], ["a", "a"]] {
        let mut unordered = bytes[..bytes.len() - 2].to_vec();
        unordered.push(1); // Recorded dependency metadata.
        unordered.extend(2u64.to_le_bytes());
        for name in names {
            text(&mut unordered, "public");
            text(&mut unordered, name);
        }
        unordered.extend([0; 17]); // Empty column/routine sets and absent OID.
        assert!(RuleCatalog::read_image(&mut unordered.as_slice(), &memory, &token).is_err());
    }
    let mut invalid = bytes.clone();
    let enabled = invalid.len() - 5;
    invalid[enabled] = 4;
    assert!(RuleCatalog::read_image(&mut invalid.as_slice(), &memory, &token).is_err());
    let mut trailing = bytes;
    trailing.push(0);
    assert!(RuleCatalog::read_image(&mut trailing.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_rule_images_retire_deep_original_and_bound_conditions_without_stack_growth() {
    let bytes = independent(10_000);
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(128 << 20);
            let token = CancellationToken::new();
            let restored = RuleCatalog::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
            let remaining = memory.reserve(memory.limit() - memory.used()).unwrap();
            assert_eq!(
                allocation_counter::measure(|| drop(restored)).count_total,
                0
            );
            drop(remaining);
            assert_eq!(memory.used(), 0);
            for end in [128, 4096, bytes.len() - 1] {
                assert!(RuleCatalog::read_image(&mut &bytes[..end], &memory, &token).is_err());
                assert_eq!(memory.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
