//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::memory::{MemoryBudget, OwnedMap};
use uqa_sql::catalog::graph_oids::GraphCatalogOids;
use uqa_storage::key_value::{KeyValueCatalog, MemoryKeyValueStore};

fn graph() -> GraphCatalogOids {
    serde_json::from_str(r#"{"namespace":20001,"label_sequence":20002,"labels":{"17":{"sequence":20003,"relation":{"relation":20004},"id_default":20007,"properties_default":20008,"not_null":{"Exact.Column":20009},"toast_table":20011,"toast_index":20012,"trigger":20017}}}"#).unwrap()
}

#[test]
fn graph_oid_changes_preserve_original_records_through_rename_label_removal_and_drop() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let original = GraphOidRegistry::try_from_iter([("saved".into(), graph())]).unwrap();
    super::super::record(&catalog, "saved", &original["saved"]).unwrap();
    assert!(labels_removed(&original, "legacy", &[17])
        .unwrap()
        .is_none());
    assert!(removed(&original, "legacy").unwrap().is_none());
    assert!(renamed(&original, "legacy", "new").unwrap().is_none());
    let unchanged = renamed(&original, "saved", "saved")
        .unwrap()
        .unwrap()
        .persist(Some(&catalog))
        .unwrap();
    assert_eq!(super::super::load(&catalog).unwrap(), original);
    assert_eq!(unchanged, original);
    let renamed = renamed(&original, "saved", "moved")
        .unwrap()
        .unwrap()
        .persist(Some(&catalog))
        .unwrap();
    assert!(renamed.get("saved").is_none());
    assert_eq!(renamed["moved"], graph());
    assert_eq!(super::super::load(&catalog).unwrap(), renamed);
    let removed_label = labels_removed(&renamed, "moved", &[17])
        .unwrap()
        .unwrap()
        .persist(Some(&catalog))
        .unwrap();
    assert_eq!(
        removed_label["moved"],
        GraphCatalogOids {
            labels: OwnedMap::new(),
            ..graph()
        }
    );
    assert_eq!(super::super::load(&catalog).unwrap(), removed_label);
    let absent = removed(&removed_label, "moved")
        .unwrap()
        .unwrap()
        .persist(Some(&catalog))
        .unwrap();
    assert!(absent.is_empty());
    assert!(super::super::load(&catalog).unwrap().is_empty());
    assert_eq!(original["saved"], graph());
    assert_eq!(renamed["moved"], graph());
}

#[test]
fn graph_oid_admission_failure_does_not_mutate_native_or_persisted_records() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let budget = MemoryBudget::new(65536);
    let mut current = GraphOidRegistry::with_budget(&budget);
    current.insert("saved".into(), graph()).unwrap();
    super::super::record(&catalog, "saved", &current["saved"]).unwrap();
    let blocked = budget.reserve(budget.available()).unwrap();
    assert!(labels_removed(&current, "saved", &[17]).is_err());
    assert!(renamed(&current, "saved", "moved").is_err());
    assert_eq!(current["saved"], graph());
    assert_eq!(super::super::load(&catalog).unwrap(), current);
    drop((blocked, current));
    assert_eq!(budget.used(), 0);
}
