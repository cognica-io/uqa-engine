//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::graph_oids::changes;
use uqa_graph::{GraphStore as _, GraphStoreHandle, LabelKind};
use uqa_sql::catalog::graph_oids::GraphOidRegistry;

#[test]
fn graph_oid_creation_keeps_legacy_absence_and_publishes_only_changed_graph_records() {
    let session = Session::new();
    let context = CatalogIdentityReservationContext {
        catalog: &session,
        session: &session,
        locks: &session,
    };
    let mut candidate = GraphStoreHandle::default();
    for name in ["legacy", "saved", "other"] {
        candidate.create_graph(name).unwrap();
    }
    let original = changes::created(
        &GraphOidRegistry::new(),
        &BTreeSet::from(["legacy".into()]),
        &candidate,
        context,
        |_| Ok(false),
    )
    .unwrap()
    .unwrap()
    .persist(None)
    .unwrap();
    assert_eq!(original.len(), 2);
    assert!(!original.contains_key("legacy"));
    let published = candidate.graph_names().unwrap().into_iter().collect();
    assert!(
        changes::created(&original, &published, &candidate, context, |_| Ok(false))
            .unwrap()
            .is_none()
    );
    candidate
        .create_label("saved", "Custom", LabelKind::Vertex)
        .unwrap();
    let updated = changes::created(&original, &published, &candidate, context, |_| Ok(false))
        .unwrap()
        .unwrap()
        .persist(None)
        .unwrap();
    assert_eq!(updated.len(), 2);
    assert!(updated.shares_definition("other", &original));
    assert!(!updated.shares_definition("saved", &original));
    assert!(!updated.contains_key("legacy"));
    assert_eq!(
        updated["saved"].labels.len(),
        original["saved"].labels.len() + 1
    );
    for (id, label) in &original["saved"].labels {
        assert_eq!(updated["saved"].labels.get(id), Some(label));
    }
    assert_eq!(updated["saved"].namespace, original["saved"].namespace);
    assert_eq!(
        updated["saved"].label_sequence,
        original["saved"].label_sequence
    );
}
