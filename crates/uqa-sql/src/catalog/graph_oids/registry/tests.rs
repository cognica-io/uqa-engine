//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::definition_capture::identities;
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryBudget, CancellationToken};

const RECORD: &str = r#"{"namespace":20001,"label_sequence":20002,"labels":{"17":{"sequence":20003,"relation":{"relation":20004,"row_type":20005,"array_type":20006},"id_default":20007,"properties_default":20008,"not_null":{"Exact.Column":20009,"removed":20010},"toast_table":20011,"toast_index":20012,"primary_key":{"index":20013,"constraint":20014},"endpoint_indexes":{"start_id":20015,"end_id":20016},"trigger":20017}}}"#;

fn graph() -> GraphCatalogOids {
    serde_json::from_str(RECORD).unwrap()
}

#[test]
fn graph_oid_roots_capture_without_enumeration_or_allocation() {
    for count in [1, 64, 4096] {
        let mut graphs = GraphOidRegistry::try_from_iter(
            (0..count).map(|index| (format!("pg_temp_graph_{index:04}"), graph())),
        )
        .unwrap();
        let expected = graphs.clone();
        let budget = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut captured = None;
        let allocations = allocation_counter::measure(|| {
            captured = Some(graphs.capture(&control).unwrap());
        });
        assert_eq!(allocations.count_total, 0);
        let required = budget.used();
        let exact = MemoryBudget::new(required);
        let independent = graphs
            .capture(&ProductionControl::new(&exact, &token, &token))
            .unwrap();
        assert_eq!(exact.used(), required);
        let insufficient = MemoryBudget::new(required - 1);
        assert!(graphs
            .capture(&ProductionControl::new(&insufficient, &token, &token))
            .is_err());
        assert_eq!(insufficient.used(), 0);
        graphs.clear();
        drop(graphs);
        assert!(captured.as_ref().unwrap().iter().eq(expected.iter()));
        assert!(independent.iter().eq(expected.iter()));
        drop((captured, independent));
        assert_eq!(budget.used(), 0);
        assert_eq!(exact.used(), 0);
    }
}

fn compare_legacy(graphs: &GraphOidRegistry, control: &ProductionControl<'_>) {
    let original: BTreeMap<_, _> = graphs.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let previous = identities::graph_oids(&original, control).unwrap();
    let current = graphs.capture(control).unwrap();
    assert_eq!(previous.len(), current.len());
    for ((name, graph), (old_name, old)) in current.iter().zip(previous.iter()) {
        assert_eq!(name, old_name);
        assert_eq!(graph.namespace, old.namespace);
        assert_eq!(graph.label_sequence, old.label_sequence);
        assert_eq!(graph.labels.len(), old.labels.len());
        for ((id, label), (old_id, old_label)) in graph.labels.iter().zip(&old.labels) {
            assert_eq!(id, old_id);
            assert_eq!(
                (
                    label.sequence,
                    label.relation,
                    label.id_default,
                    label.properties_default,
                    label.toast_table,
                    label.toast_index,
                    label.primary_key,
                    label.endpoint_indexes,
                    label.trigger
                ),
                (
                    old_label.sequence,
                    old_label.relation,
                    old_label.id_default,
                    old_label.properties_default,
                    old_label.toast_table,
                    old_label.toast_index,
                    old_label.primary_key,
                    old_label.endpoint_indexes,
                    old_label.trigger
                )
            );
            assert!(label
                .not_null
                .iter()
                .eq(old_label.not_null.iter().map(|(k, v)| (k, v))));
        }
    }
    assert_eq!(
        serde_json::to_string(graphs).unwrap(),
        serde_json::to_string(&original).unwrap()
    );
}

#[test]
fn graph_oid_copies_admit_exact_nested_allocations_and_keep_legacy_records() {
    let original = graph();
    assert_eq!(serde_json::to_string(&original).unwrap(), RECORD);
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut copied = None;
    let allocations = allocation_counter::measure(|| {
        copied = Some(original.copy_definition(&control).unwrap());
    });
    let bytes = budget.used();
    assert_eq!(allocations.bytes_current, i64::try_from(bytes).unwrap());
    assert_eq!(&**copied.as_ref().unwrap(), &original);
    drop(copied);
    assert_eq!(budget.used(), 0);
    let small = MemoryBudget::new(bytes - 1);
    assert!(original
        .copy_definition(&ProductionControl::new(&small, &token, &token))
        .is_err());
    assert_eq!(small.used(), 0);
    let graphs = GraphOidRegistry::try_from_iter([("Exact.Graph".into(), original)]).unwrap();
    compare_legacy(&graphs, &control);
    assert_eq!(budget.used(), 0);
    token.cancel();
    assert!(matches!(
        graphs.capture(&control),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn selected_graph_oid_publication_shares_unrelated_definitions() {
    for count in [1, 32, 512] {
        let mut graphs = GraphOidRegistry::try_from_iter(
            (0..count).map(|index| (format!("graph_{index:04}"), graph())),
        )
        .unwrap();
        let mut large = graph();
        large
            .labels
            .get_mut(&17)
            .unwrap()
            .not_null
            .insert("large".repeat(1 << 15), 20018);
        graphs.insert("large".into(), large).unwrap();
        let original = graphs.clone();
        let allocations = allocation_counter::measure(|| {
            graphs
                .edit("graph_0000", |graph| {
                    graph.labels.get_mut(&17).unwrap().trigger = 30000;
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
        for name in original.keys().filter(|name| *name != "graph_0000") {
            assert!(graphs.shares_definition(name, &original));
        }
        assert_eq!(original["graph_0000"].labels[&17].trigger, 20017);
        assert_eq!(graphs["graph_0000"].labels[&17].trigger, 30000);
        let budget = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        compare_legacy(&graphs, &ProductionControl::new(&budget, &token, &token));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn denied_graph_oid_publication_preserves_captured_roots_and_releases_admission() {
    let native = MemoryBudget::new(65536);
    let mut graphs = GraphOidRegistry::with_budget(&native);
    graphs.insert("saved".into(), graph()).unwrap();
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let saved = graphs
        .capture(&ProductionControl::new(&budget, &token, &token))
        .unwrap();
    let blocked = native.reserve(native.available()).unwrap();
    assert!(graphs
        .edit("saved", |graph| {
            graph.namespace = 30000;
            Ok(())
        })
        .is_err());
    assert_eq!(graphs["saved"], graph());
    assert_eq!(saved.get("saved"), Some(&graph()));
    drop((blocked, graphs));
    assert!(native.used() > 0);
    drop(saved);
    assert_eq!(native.used(), 0);
    assert_eq!(budget.used(), 0);
}
