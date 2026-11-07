//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{
    EventEnableMode, Expr, TriggerDeferrability, TriggerTiming, TriggerTransitionRelation,
};
use uqa_core::memory::ProductionControl;

mod input;
mod retention;
mod wire;

fn native() -> StoredTrigger {
    StoredTrigger {
        definition: CreateTrigger {
            name: "watch".into(),
            table: "s.t".into(),
            function: "s.fire".into(),
            arguments: vec!["original\0argument".into(), "".into()],
            constraint: true,
            referenced_table: Some("s.parent".into()),
            deferrability: TriggerDeferrability::InitiallyDeferred,
            row: true,
            timing: TriggerTiming::After,
            events: vec![TriggerEvent::Update, TriggerEvent::Delete],
            update_columns: vec!["Value.Name".into()],
            transition_relations: vec![TriggerTransitionRelation {
                name: "old.rows".into(),
                is_new: false,
                is_table: true,
            }],
            when: Some(Expr::IsNull {
                expr: Box::new(Expr::QualifiedColumn {
                    qualifier: "new".into(),
                    column: "Value.Name".into(),
                }),
                negated: true,
            }),
            or_replace: false,
        },
        function_object_id: Some([1; 16]),
        enabled: EventEnableMode::Replica,
        object_id: Some([2; 16]),
        constraint_name: Some("check_watch".into()),
        catalog_oid: Some(65536),
        constraint_catalog_oid: Some(65537),
    }
}

fn catalog(value: StoredTrigger) -> TriggerCatalog {
    let mut source = TriggerCatalog::new();
    source
        .insert_event(RelationIdentity::new("s", "t"), "watch".into(), value)
        .unwrap();
    source
}

fn collect(mut reader: TriggerImageReader<'_>, size: usize) -> Vec<u8> {
    let mut result = Vec::new();
    let mut bytes = vec![0; size];
    loop {
        let count = reader.read(&mut bytes).unwrap();
        assert!(count <= 16384);
        if count == 0 {
            break;
        }
        result.extend_from_slice(&bytes[..count]);
    }
    assert_eq!(reader.read(&mut bytes).unwrap(), 0);
    result
}

fn encode(source: &TriggerCatalog, size: usize) -> Vec<u8> {
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let captured = source
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let bytes = collect(captured.image_reader(&memory, &token).unwrap(), size);
    drop(captured);
    assert_eq!(memory.used(), 0);
    bytes
}

fn observe(source: &TriggerCatalog) -> serde_json::Value {
    serde_json::to_value(source.iter().collect::<Vec<_>>()).unwrap()
}

#[test]
fn native_trigger_images_match_literal_fields_and_native_observations() {
    let expected = wire::fixture().bytes;
    let source = catalog(native());
    for size in [1, 7, 64, 20000] {
        assert_eq!(encode(&source, size), expected);
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut decoded = None;
    let allocated = allocation_counter::measure(|| {
        decoded =
            Some(TriggerCatalog::read_image(&mut expected.as_slice(), &memory, &token).unwrap());
    });
    // Each of the two native maps retains one entry and one tree node. Core's payload allowance excludes the two reference counters of each shared allocation.
    let reference_counts = 4 * 2 * size_of::<usize>();
    assert_eq!(
        allocated.bytes_current,
        i64::try_from(memory.used() + reference_counts).unwrap()
    );
    let decoded = decoded.unwrap();
    assert_eq!(observe(&decoded), observe(&source));
    assert_eq!(encode(&decoded, 31), expected);
    drop(decoded);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_trigger_images_preserve_optional_metadata_and_every_enum() {
    for mask in 0..128 {
        let mut value = native();
        if mask & 1 == 0 {
            value.function_object_id = None;
        }
        if mask & 2 == 0 {
            value.object_id = None;
        }
        if mask & 4 == 0 {
            value.catalog_oid = None;
        }
        if mask & 8 == 0 {
            value.constraint_catalog_oid = None;
        }
        if mask & 16 == 0 {
            value.constraint_name = None;
        }
        if mask & 32 == 0 {
            value.definition.referenced_table = None;
        }
        if mask & 64 == 0 {
            value.definition.when = None;
        }
        value.enabled = [
            EventEnableMode::Origin,
            EventEnableMode::Disabled,
            EventEnableMode::Replica,
            EventEnableMode::Always,
        ][mask % 4];
        value.definition.timing = [
            TriggerTiming::Before,
            TriggerTiming::After,
            TriggerTiming::InsteadOf,
        ][mask % 3];
        value.definition.deferrability = [
            TriggerDeferrability::NotDeferrable,
            TriggerDeferrability::InitiallyImmediate,
            TriggerDeferrability::InitiallyDeferred,
        ][mask % 3];
        value.definition.events = vec![
            TriggerEvent::Insert,
            TriggerEvent::Update,
            TriggerEvent::Delete,
            TriggerEvent::Truncate,
        ];
        value.definition.constraint = mask & 1 != 0;
        value.definition.row = mask & 2 != 0;
        value.definition.or_replace = mask & 4 != 0;
        value.definition.transition_relations[0].is_new = mask & 8 != 0;
        value.definition.transition_relations[0].is_table = mask & 16 != 0;
        let source = catalog(value);
        let bytes = encode(&source, 31);
        let memory = MemoryBudget::new(1 << 20);
        let decoded =
            TriggerCatalog::read_image(&mut bytes.as_slice(), &memory, &CancellationToken::new())
                .unwrap();
        assert_eq!(observe(&decoded), observe(&source), "mask {mask}");
        assert_eq!(encode(&decoded, 7), bytes);
        drop(decoded);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_trigger_images_preserve_empty_relations_and_quoted_membership() {
    let mut source = TriggerCatalog::new();
    source
        .insert(RelationIdentity::new("a", "empty"), EventDefinitions::new())
        .unwrap();
    let relation = RelationIdentity::new("Dot.Schema", "Quote\".Table");
    for name in ["a", "z"] {
        let mut value = native();
        value.definition.name = name.into();
        value.definition.table = relation.qualified_name();
        value.object_id = None;
        value.catalog_oid = None;
        value.constraint_catalog_oid = None;
        source
            .insert_event(relation.clone(), name.into(), value)
            .unwrap();
    }
    let bytes = encode(&source, 1);
    let memory = MemoryBudget::new(1 << 20);
    let decoded =
        TriggerCatalog::read_image(&mut bytes.as_slice(), &memory, &CancellationToken::new())
            .unwrap();
    assert_eq!(decoded.len(), 2);
    assert!(decoded[&RelationIdentity::new("a", "empty")].is_empty());
    assert_eq!(decoded[&relation].len(), 2);
    assert_eq!(encode(&decoded, 4096), bytes);
    drop(decoded);
    assert_eq!(memory.used(), 0);
    let empty = b"UQATRG01\0\0\0\0\0\0\0\0";
    let zero = MemoryBudget::new(0);
    assert!(
        TriggerCatalog::read_image(&mut empty.as_slice(), &zero, &CancellationToken::new())
            .unwrap()
            .is_empty()
    );
}
