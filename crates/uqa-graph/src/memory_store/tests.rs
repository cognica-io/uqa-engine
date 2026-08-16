//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn build_basic_graph() -> MemoryGraphStore {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    store.add_vertex(Vertex::new(1, "person"), "g").unwrap();
    store.add_vertex(Vertex::new(2, "person"), "g").unwrap();
    store.add_vertex(Vertex::new(3, "company"), "g").unwrap();
    store.add_edge(Edge::new(10, 1, 2, "knows"), "g").unwrap();
    store
        .add_edge(Edge::new(11, 1, 3, "works_at"), "g")
        .unwrap();
    store
}

#[test]
fn neighbors_filter_by_label() {
    let store = build_basic_graph();
    let mut out = store
        .neighbors(1, Some("knows"), Direction::Out, "g")
        .unwrap();
    out.sort_unstable();
    assert_eq!(out, vec![2]);
}

#[test]
fn missing_query_vertex_is_not_an_empty_neighborhood() {
    let store = build_basic_graph();
    for result in [
        store.neighbors(999, None, Direction::Out, "g").map(|_| ()),
        store.out_edge_ids(999, "g").map(|_| ()),
        store.in_edge_ids(999, "g").map(|_| ()),
    ] {
        assert!(matches!(result, Err(GraphStoreError::InvalidQuery(_))));
    }
}

#[test]
fn edge_endpoints_must_belong_to_the_target_graph() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("left");
    store.create_graph("right");
    store.add_vertex(Vertex::new(1, "v"), "left").unwrap();
    store.add_vertex(Vertex::new(2, "v"), "right").unwrap();

    assert!(matches!(
        store.add_edge(Edge::new(10, 1, 2, "cross"), "left"),
        Err(GraphStoreError::InvalidMutation(message))
            if message.contains("outside graph")
    ));
}

#[test]
fn dangling_membership_records_surface_as_corruption() {
    let mut missing_edge = build_basic_graph();
    missing_edge.remove_edge_record_for_corruption_test(10);
    assert!(matches!(
        missing_edge.neighbors(1, None, Direction::Out, "g"),
        Err(GraphStoreError::CorruptGraph(_))
    ));
    assert!(matches!(
        missing_edge.edges_in_graph("g"),
        Err(GraphStoreError::CorruptGraph(_))
    ));

    let mut missing_vertex = build_basic_graph();
    missing_vertex.remove_vertex_record_for_corruption_test(2);
    assert!(matches!(
        missing_vertex.vertices_in_graph("g"),
        Err(GraphStoreError::CorruptGraph(_))
    ));
    assert!(matches!(
        missing_vertex.vertex_ids_by_label("person", "g"),
        Err(GraphStoreError::CorruptGraph(_))
    ));
}

#[test]
fn vertex_ids_by_label_uses_label_membership() {
    let store = build_basic_graph();
    assert_eq!(
        store.vertex_ids_by_label("person", "g").unwrap(),
        vec![1, 2]
    );
    assert_eq!(store.vertex_ids_by_label("company", "g").unwrap(), vec![3]);
    assert!(store
        .vertex_ids_by_label("missing", "g")
        .unwrap()
        .is_empty());
}

#[test]
fn neighbors_in_direction() {
    let store = build_basic_graph();
    let inn = store.neighbors(2, None, Direction::In, "g").unwrap();
    assert_eq!(inn, vec![1]);
}

#[test]
fn neighbors_both_dedupes() {
    let mut store = build_basic_graph();
    // Self-loop the other way.
    store.add_edge(Edge::new(12, 2, 1, "knows"), "g").unwrap();
    let mut out = store
        .neighbors(1, Some("knows"), Direction::Both, "g")
        .unwrap();
    out.sort_unstable();
    assert_eq!(out, vec![2]);
}

#[test]
fn drop_graph_releases_orphan_records() {
    let mut store = build_basic_graph();
    store.drop_graph("g");
    assert!(store.get_vertex(1).is_none());
    assert!(store.get_edge(10).is_none());
    assert!(!store.has_graph("g"));
}

#[test]
fn membership_tracks_multiple_graphs() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("a");
    store.create_graph("b");
    store.add_vertex(Vertex::new(1, "node"), "a").unwrap();
    store.add_vertex(Vertex::new(1, "node"), "b").unwrap();
    let mship = store.vertex_graphs(1);
    assert!(mship.contains("a") && mship.contains("b"));
    store.drop_graph("a");
    // Vertex 1 still belongs to "b", so it must survive.
    assert!(store.get_vertex(1).is_some());
    assert_eq!(
        store.vertex_graphs(1).into_iter().collect::<Vec<_>>(),
        vec!["b".to_string()]
    );
}

#[test]
fn graph_algebra_union_intersect_difference() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g1");
    store.create_graph("g2");
    store.add_vertex(Vertex::new(1, "v"), "g1").unwrap();
    store.add_vertex(Vertex::new(2, "v"), "g1").unwrap();
    store.add_vertex(Vertex::new(2, "v"), "g2").unwrap();
    store.add_vertex(Vertex::new(3, "v"), "g2").unwrap();

    store.union_graphs("g1", "g2", "u").unwrap();
    let u_ids: Vec<_> = store
        .vertex_ids_in_graph("u")
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(u_ids, vec![1, 2, 3]);

    store.intersect_graphs("g1", "g2", "i").unwrap();
    let i_ids: Vec<_> = store
        .vertex_ids_in_graph("i")
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(i_ids, vec![2]);

    store.difference_graphs("g1", "g2", "d").unwrap();
    let d_ids: Vec<_> = store
        .vertex_ids_in_graph("d")
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(d_ids, vec![1]);
}

#[test]
fn next_id_advances() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    assert_eq!(store.next_vertex_id().unwrap(), 1);
    assert_eq!(store.next_vertex_id().unwrap(), 2);
    assert_eq!(store.next_edge_id().unwrap(), 1);
    store.add_vertex(Vertex::new(99, "v"), "g").unwrap();
    assert_eq!(store.next_vertex_id().unwrap(), 100);
}

#[test]
fn allocate_ids_follow_age_graphid_scheme() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    // First user vertex label -> label id 3, sequence 1.
    assert_eq!(
        store.allocate_vertex_id("Person", "g").unwrap(),
        844_424_930_131_969
    );
    assert_eq!(
        store.allocate_vertex_id("Person", "g").unwrap(),
        844_424_930_131_970
    );
    // Edge labels share the same per-graph label counter -> 4.
    assert_eq!(
        store.allocate_edge_id("KNOWS", "g").unwrap(),
        1_125_899_906_842_625
    );
    // Next new vertex label continues the shared counter -> 5.
    assert_eq!(
        store.allocate_vertex_id("City", "g").unwrap(),
        1_407_374_883_553_281
    );
    // Unlabeled entities land in the reserved label ids 1 / 2.
    assert_eq!(
        store.allocate_vertex_id("", "g").unwrap(),
        make_graphid(1, 1).unwrap()
    );
    assert_eq!(
        store.allocate_edge_id("", "g").unwrap(),
        make_graphid(2, 1).unwrap()
    );
    // Sequences are per label.
    assert_eq!(
        store.allocate_edge_id("KNOWS", "g").unwrap(),
        make_graphid(4, 2).unwrap()
    );
}

#[test]
fn label_registry_rebuild_from_ids_self_heals() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    store
        .add_vertex(Vertex::new(make_graphid(3, 7).unwrap(), "Person"), "g")
        .unwrap();
    store
        .add_edge(
            Edge::new(
                make_graphid(4, 2).unwrap(),
                make_graphid(3, 7).unwrap(),
                make_graphid(3, 7).unwrap(),
                "KNOWS",
            ),
            "g",
        )
        .unwrap();
    store.rebuild_label_registry_from_ids("g");
    // New allocations continue after the observed watermarks.
    assert_eq!(
        store.allocate_vertex_id("Person", "g").unwrap(),
        make_graphid(3, 8).unwrap()
    );
    assert_eq!(
        store.allocate_edge_id("KNOWS", "g").unwrap(),
        make_graphid(4, 3).unwrap()
    );
    // A brand-new label picks the next free label id (5).
    assert_eq!(
        store.allocate_vertex_id("City", "g").unwrap(),
        make_graphid(5, 1).unwrap()
    );
}

#[test]
fn label_registry_survives_round_trip_via_import() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    store.allocate_vertex_id("Person", "g").unwrap();
    let registry = store.label_registry("g");
    let json = serde_json::to_string(&registry).unwrap();
    let restored: GraphLabelRegistry = serde_json::from_str(&json).unwrap();

    let mut fresh = MemoryGraphStore::new();
    fresh.create_graph("g");
    fresh.import_label_registry("g", &restored);
    assert_eq!(
        fresh.allocate_vertex_id("Person", "g").unwrap(),
        make_graphid(3, 2).unwrap()
    );
}

#[test]
fn drop_graph_resets_label_registry() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    let first = store.allocate_vertex_id("Person", "g").unwrap();
    store.drop_graph("g");
    store.create_graph("g");
    assert_eq!(store.allocate_vertex_id("Person", "g").unwrap(), first);
}

#[test]
fn graph_id_allocation_rejects_missing_graph_and_exhaustion() {
    let mut store = MemoryGraphStore::new();
    assert!(matches!(
        store.allocate_vertex_id("Person", "missing"),
        Err(GraphStoreError::UnknownGraph(_))
    ));
    assert!(make_graphid(MAX_GRAPHID_LABEL_ID + 1, 1).is_err());
    assert!(make_graphid(1, MAX_GRAPHID_SEQUENCE + 1).is_err());
}

#[test]
fn label_kinds_are_recorded_and_enforced_like_age() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    store.allocate_vertex_id("Person", "g").unwrap();
    store.allocate_edge_id("KNOWS", "g").unwrap();
    assert_eq!(
        store.graph_label_kind("g", "Person").unwrap(),
        Some(LabelKind::Vertex)
    );
    assert_eq!(
        store.graph_label_kind("g", "KNOWS").unwrap(),
        Some(LabelKind::Edge)
    );
    assert_eq!(
        store.graph_label_kind("g", "_ag_label_edge").unwrap(),
        Some(LabelKind::Edge)
    );
    assert_eq!(store.graph_label_kind("g", "Nope").unwrap(), None);
    let err = store.allocate_edge_id("Person", "g").unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid graph mutation: label Person is for vertices, not edges"
    );
    let err = store.allocate_vertex_id("KNOWS", "g").unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid graph mutation: label KNOWS is for edges, not vertices"
    );
}

#[test]
fn label_registry_lists_default_labels_first_then_user_labels_by_id() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    store.allocate_vertex_id("Zeta", "g").unwrap();
    store.allocate_edge_id("Alpha", "g").unwrap();
    assert_eq!(
        store.create_label("g", "Empty", LabelKind::Vertex).unwrap(),
        Some(5)
    );
    assert_eq!(
        store.create_label("g", "Empty", LabelKind::Edge).unwrap(),
        None
    );
    assert_eq!(
        store
            .create_label("g", "_ag_label_vertex", LabelKind::Vertex)
            .unwrap(),
        None
    );
    assert!(matches!(
        store.create_label("missing", "X", LabelKind::Vertex),
        Err(GraphStoreError::UnknownGraph(_))
    ));
    let labels = store.graph_labels("g").unwrap();
    let shaped: Vec<(&str, u32, LabelKind, u64)> = labels
        .iter()
        .map(|label| {
            (
                label.name.as_str(),
                label.id,
                label.kind,
                label.last_sequence,
            )
        })
        .collect();
    assert_eq!(
        shaped,
        vec![
            ("_ag_label_vertex", 1, LabelKind::Vertex, 0),
            ("_ag_label_edge", 2, LabelKind::Edge, 0),
            ("Zeta", 3, LabelKind::Vertex, 1),
            ("Alpha", 4, LabelKind::Edge, 1),
            ("Empty", 5, LabelKind::Vertex, 0),
        ]
    );
    // A pre-registered label keeps its id when entities arrive.
    assert_eq!(
        store.allocate_vertex_id("Empty", "g").unwrap(),
        make_graphid(5, 1).unwrap()
    );
}

#[test]
fn drop_label_removes_entities_and_releases_the_label() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    let a = store.allocate_vertex_id("Person", "g").unwrap();
    let b = store.allocate_vertex_id("Person", "g").unwrap();
    let c = store.allocate_vertex_id("City", "g").unwrap();
    store.add_vertex(Vertex::new(a, "Person"), "g").unwrap();
    store.add_vertex(Vertex::new(b, "Person"), "g").unwrap();
    store.add_vertex(Vertex::new(c, "City"), "g").unwrap();
    let knows = store.allocate_edge_id("KNOWS", "g").unwrap();
    let lives = store.allocate_edge_id("LIVES_IN", "g").unwrap();
    store
        .add_edge(Edge::new(knows, a, b, "KNOWS"), "g")
        .unwrap();
    store
        .add_edge(Edge::new(lives, a, c, "LIVES_IN"), "g")
        .unwrap();

    // Labels were allocated Person(3), City(4), KNOWS(5), LIVES_IN(6).
    assert_eq!(
        store.drop_label("g", "KNOWS").unwrap(),
        Some((5, LabelKind::Edge))
    );
    assert_eq!(store.edges_in_graph("g").unwrap().len(), 1);
    assert_eq!(store.vertex_ids_in_graph("g").unwrap().len(), 3);

    assert_eq!(
        store.drop_label("g", "Person").unwrap(),
        Some((3, LabelKind::Vertex))
    );
    assert_eq!(store.vertex_ids_in_graph("g").unwrap().len(), 1);
    // The incident LIVES_IN edge left with its Person endpoint.
    assert!(store.edges_in_graph("g").unwrap().is_empty());
    assert_eq!(store.drop_label("g", "Person").unwrap(), None);
    assert_eq!(store.drop_label("g", "_ag_label_vertex").unwrap(), None);
    assert!(matches!(
        store.drop_label("missing", "Person"),
        Err(GraphStoreError::UnknownGraph(_))
    ));
    let names: Vec<String> = store
        .graph_labels("g")
        .unwrap()
        .into_iter()
        .map(|label| label.name)
        .collect();
    assert_eq!(
        names,
        vec!["_ag_label_vertex", "_ag_label_edge", "City", "LIVES_IN"]
    );
    // Released label ids are not reused; the shared counter keeps moving.
    assert_eq!(
        store.allocate_vertex_id("Person", "g").unwrap(),
        make_graphid(7, 1).unwrap()
    );
}

#[test]
fn rename_graph_moves_partition_membership_and_registry() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("old");
    store.create_graph("other");
    let a = store.allocate_vertex_id("Person", "old").unwrap();
    store.add_vertex(Vertex::new(a, "Person"), "old").unwrap();
    assert!(matches!(
        store.rename_graph("old", "other"),
        Err(GraphStoreError::InvalidMutation(_))
    ));
    assert!(matches!(
        store.rename_graph("missing", "new"),
        Err(GraphStoreError::UnknownGraph(_))
    ));
    store.rename_graph("old", "new").unwrap();
    assert!(!store.has_graph("old"));
    assert!(store.has_graph("new"));
    assert_eq!(store.vertex_graphs(a), BTreeSet::from(["new".to_string()]));
    assert_eq!(
        store.vertex_ids_in_graph("new").unwrap(),
        BTreeSet::from([a])
    );
    assert_eq!(
        store.graph_label_kind("new", "Person").unwrap(),
        Some(LabelKind::Vertex)
    );
    assert_eq!(
        store.allocate_vertex_id("Person", "new").unwrap(),
        make_graphid(3, 2).unwrap()
    );
}

#[test]
fn legacy_registries_without_kinds_learn_them_from_entities() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    store
        .add_vertex(Vertex::new(make_graphid(3, 1).unwrap(), "Person"), "g")
        .unwrap();
    store
        .add_edge(
            Edge::new(
                make_graphid(4, 1).unwrap(),
                make_graphid(3, 1).unwrap(),
                make_graphid(3, 1).unwrap(),
                "KNOWS",
            ),
            "g",
        )
        .unwrap();
    let legacy: GraphLabelRegistry = serde_json::from_str(
        r#"{"labels":{"Person":3,"KNOWS":4},"sequences":{"3":1,"4":1},"next_label_id":5}"#,
    )
    .unwrap();
    assert!(legacy.kinds.is_empty());
    store.import_label_registry("g", &legacy);
    store.rebuild_label_registry_from_ids("g");
    assert_eq!(
        store.graph_label_kind("g", "Person").unwrap(),
        Some(LabelKind::Vertex)
    );
    assert_eq!(
        store.graph_label_kind("g", "KNOWS").unwrap(),
        Some(LabelKind::Edge)
    );
}

#[test]
fn reserved_label_names_resolve_to_the_default_labels() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    assert_eq!(
        store.allocate_vertex_id("_ag_label_vertex", "g").unwrap(),
        make_graphid(1, 1).unwrap()
    );
    assert_eq!(
        store.allocate_edge_id("_ag_label_edge", "g").unwrap(),
        make_graphid(2, 1).unwrap()
    );
    let err = store.allocate_edge_id("_ag_label_vertex", "g").unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid graph mutation: label _ag_label_vertex is for vertices, not edges"
    );
    let err = store.allocate_vertex_id("_ag_label_edge", "g").unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid graph mutation: label _ag_label_edge is for edges, not vertices"
    );
    // The reserved names never become user labels.
    let names: Vec<String> = store
        .graph_labels("g")
        .unwrap()
        .into_iter()
        .map(|label| label.name)
        .collect();
    assert_eq!(names, vec!["_ag_label_vertex", "_ag_label_edge"]);
    assert_eq!(
        store.allocate_vertex_id("Person", "g").unwrap(),
        make_graphid(3, 1).unwrap()
    );
}

#[test]
fn legacy_labels_without_kind_and_entities_report_consistently() {
    let mut store = MemoryGraphStore::new();
    store.create_graph("g");
    let legacy: GraphLabelRegistry =
        serde_json::from_str(r#"{"labels":{"Ghost":3},"sequences":{"3":2},"next_label_id":4}"#)
            .unwrap();
    store.import_label_registry("g", &legacy);
    store.rebuild_label_registry_from_ids("g");
    // No entity can tell the kind, so the catalog and the drop path agree
    // on the vertex default until the label is used again.
    assert_eq!(
        store.graph_label_kind("g", "Ghost").unwrap(),
        Some(LabelKind::Vertex)
    );
    let ghost = store
        .graph_labels("g")
        .unwrap()
        .into_iter()
        .find(|label| label.name == "Ghost")
        .unwrap();
    assert_eq!(
        (ghost.id, ghost.kind, ghost.last_sequence),
        (3, LabelKind::Vertex, 2)
    );
    // First use records the kind, either way.
    assert_eq!(
        store.allocate_edge_id("Ghost", "g").unwrap(),
        make_graphid(3, 3).unwrap()
    );
    assert_eq!(
        store.graph_label_kind("g", "Ghost").unwrap(),
        Some(LabelKind::Edge)
    );
    assert_eq!(
        store.drop_label("g", "Ghost").unwrap(),
        Some((3, LabelKind::Edge))
    );
}
