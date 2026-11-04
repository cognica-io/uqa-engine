//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn graph() -> GraphCatalogOids {
    GraphCatalogOids {
        namespace: 20001,
        label_sequence: 20002,
        labels: uqa_core::memory::OwnedMap::from_iter([(
            17,
            LabelCatalogOids {
                sequence: 20003,
                relation: RelationCatalogOids {
                    relation: 20004,
                    row_type: Some(20005),
                    array_type: Some(20006),
                    rule: None,
                },
                id_default: 20007,
                properties_default: 20008,
                not_null: uqa_core::memory::OwnedMap::from_iter([
                    ("Exact.Column".into(), 20009),
                    ("removed".into(), 20010),
                ]),
                toast_table: 20011,
                toast_index: 20012,
                primary_key: Some(IndexConstraintOids {
                    index: 20013,
                    constraint: 20014,
                }),
                endpoint_indexes: Some(EndpointIndexOids {
                    start_id: 20015,
                    end_id: 20016,
                }),
                trigger: 20017,
            },
        )]),
    }
}

#[test]
fn captured_sequence_identities_preserve_exact_names_oid_claims_and_persistence() {
    let memory = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let names = [
        RelationIdentity::new("Quoted.Schema", "Sequence.Name"),
        RelationIdentity::new("public", "plain"),
        RelationIdentity::new("pg_temp_3", "local"),
    ];
    let objects = BTreeMap::from([(names[0].clone(), [1; 16]), (names[1].clone(), [2; 16])]);
    let oids = BTreeMap::from([([1; 16], 20001), ([2; 16], u32::MAX)]);
    let persistence = BTreeMap::from([
        (names[0].clone(), RelationPersistence::Permanent),
        (names[1].clone(), RelationPersistence::Unlogged),
        (names[2].clone(), RelationPersistence::Temporary),
    ]);
    macro_rules! exact {
        ($copy:ident, $source:expr) => {{
            let saved = $copy(&$source, &control).unwrap();
            assert!(saved.iter().map(|(k, v)| (k, v)).eq($source.iter()));
            drop(saved);
            assert_eq!(memory.used(), 0);
        }};
    }
    exact!(sequence_objects, objects);
    exact!(sequence_oids, oids);
    exact!(sequence_persistence, persistence);
}

#[test]
fn captured_graph_catalog_identities_keep_every_allocated_label_object() {
    let source = BTreeMap::from([("Exact.Graph".into(), graph())]);
    let memory = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut saved = None;
    let measured =
        allocation_counter::measure(|| saved = Some(graph_oids(&source, &control).unwrap()));
    let saved = saved.unwrap();
    assert_eq!(
        measured.bytes_current,
        i64::try_from(memory.used()).unwrap()
    );
    assert_eq!(saved[0].0, "Exact.Graph");
    let original = &source["Exact.Graph"];
    let captured = &saved[0].1;
    assert_eq!(captured.namespace, original.namespace);
    assert_eq!(captured.label_sequence, original.label_sequence);
    assert_eq!(captured.labels.len(), original.labels.len());
    let before = &original.labels[&17];
    let (id, after) = &captured.labels[0];
    assert_eq!(*id, 17);
    assert_eq!(
        (
            after.sequence,
            after.relation,
            after.id_default,
            after.properties_default,
            after.toast_table,
            after.toast_index,
            after.primary_key,
            after.endpoint_indexes,
            after.trigger
        ),
        (
            before.sequence,
            before.relation,
            before.id_default,
            before.properties_default,
            before.toast_table,
            before.toast_index,
            before.primary_key,
            before.endpoint_indexes,
            before.trigger
        )
    );
    assert!(after
        .not_null
        .iter()
        .map(|(k, v)| (k, v))
        .eq(before.not_null.iter()));
    drop(source);
    assert_eq!(saved[0].1.labels[0].1.trigger, 20017);
    drop(saved);
    assert_eq!(memory.used(), 0);
}

#[test]
fn identity_capture_rejects_denied_names_nested_constraints_and_original_cancellation() {
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let names = BTreeMap::from([(RelationIdentity::new("s", "x".repeat(32768)), [1; 16])]);
    let mut value = graph();
    value
        .labels
        .get_mut(&17)
        .unwrap()
        .not_null
        .insert("x".repeat(32768), 20018);
    let graphs = BTreeMap::from([("graph".into(), value)]);
    assert_eq!(
        sequence_objects(&names, &control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    assert_eq!(
        graph_oids(&graphs, &control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    token.cancel();
    assert!(matches!(
        sequence_objects(&names, &control),
        Err(SQLError::Cancelled(_))
    ));
    assert!(matches!(
        graph_oids(&graphs, &control),
        Err(SQLError::Cancelled(_))
    ));
    assert!(matches!(
        sequence_oids(&BTreeMap::new(), &control),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}
