//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::TruncateTarget;
use std::{cell::RefCell, collections::BTreeMap};

#[derive(Default)]
struct Catalog {
    hierarchy: BTreeMap<String, Vec<String>>,
    references: BTreeMap<String, Vec<String>>,
    partitioned: BTreeSet<String>,
    events: RefCell<Vec<String>>,
    reference_error: Option<String>,
}
impl TruncateCatalog for Catalog {
    fn try_resolve_visible_relation_kind(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        self.events.borrow_mut().push(format!("resolve:{name}"));
        Ok(self
            .hierarchy
            .contains_key(name)
            .then(|| (name.into(), "table")))
    }
    fn is_partitioned(&self, table: &str) -> Result<bool, String> {
        self.events.borrow_mut().push(format!("partition:{table}"));
        Ok(self.partitioned.contains(table))
    }
    fn hierarchy_scan_tables(
        &self,
        table: &str,
        descendants: bool,
    ) -> Result<Vec<String>, SQLError> {
        self.events.borrow_mut().push(format!("hierarchy:{table}"));
        Ok(if descendants {
            self.hierarchy[table].clone()
        } else {
            vec![table.into()]
        })
    }
    fn referrers_to(&self, table: &str) -> Result<Vec<String>, String> {
        self.events.borrow_mut().push(format!("references:{table}"));
        if self.reference_error.as_deref() == Some(table) {
            return Err("catalog read failed".into());
        }
        Ok(self.references.get(table).cloned().unwrap_or_default())
    }
}
fn target(table: &str, include_descendants: bool) -> TruncateTarget {
    TruncateTarget {
        table: table.into(),
        include_descendants,
    }
}
fn catalog(tables: &[&str]) -> Catalog {
    Catalog {
        hierarchy: tables
            .iter()
            .map(|name| ((*name).into(), vec![(*name).into()]))
            .collect(),
        ..Catalog::default()
    }
}

#[test]
fn hierarchy_targets_keep_first_trigger_order_and_explicit_privilege_targets() {
    let mut catalog = catalog(&["z_parent", "b_child"]);
    catalog
        .hierarchy
        .insert("z_parent".into(), vec!["z_parent".into(), "b_child".into()]);
    let targets = resolve_truncate_targets(
        &catalog,
        &[
            target("z_parent", true),
            target("b_child", false),
            target("z_parent", true),
        ],
        false,
    )
    .unwrap();
    assert_eq!(targets.trigger_order, ["z_parent", "b_child"]);
    assert_eq!(
        targets.all,
        BTreeSet::from(["b_child".into(), "z_parent".into()])
    );
    assert_eq!(targets.privilege_targets, targets.all);
}

#[test]
fn cascade_expands_breadth_first_and_dependency_order_visits_cycles_once() {
    let mut catalog = catalog(&["root", "left", "right", "leaf"]);
    catalog.references = BTreeMap::from([
        ("root".into(), vec!["left".into(), "right".into()]),
        ("left".into(), vec!["leaf".into()]),
        ("right".into(), vec!["leaf".into()]),
        ("leaf".into(), vec!["root".into()]),
    ]);
    let targets = resolve_truncate_targets(&catalog, &[target("root", true)], true).unwrap();
    assert_eq!(targets.trigger_order, ["root", "left", "right", "leaf"]);
    assert_eq!(targets.privilege_targets, targets.all);
    catalog.events.borrow_mut().clear();
    assert_eq!(
        truncate_dependency_order(&catalog, &targets).unwrap(),
        ["leaf", "left", "right", "root"]
    );
    assert_eq!(
        *catalog.events.borrow(),
        [
            "references:root",
            "references:left",
            "references:leaf",
            "references:right"
        ]
    );
}

#[test]
fn only_partition_rejection_precedes_hierarchy_expansion() {
    let mut catalog = catalog(&["partitioned"]);
    catalog.partitioned.insert("partitioned".into());
    let error =
        resolve_truncate_targets(&catalog, &[target("partitioned", false)], false).unwrap_err();
    assert!(
        matches!(error, SQLError::Routine { sqlstate, message } if sqlstate == "42809" && message == "cannot truncate only a partitioned table")
    );
    assert_eq!(
        *catalog.events.borrow(),
        ["resolve:partitioned", "partition:partitioned"]
    );
}

#[test]
fn restrict_and_dependency_reads_preserve_the_first_catalog_diagnostic() {
    let mut catalog = catalog(&["a", "z"]);
    catalog
        .references
        .insert("a".into(), vec!["external".into()]);
    let targets =
        resolve_truncate_targets(&catalog, &[target("z", true), target("a", true)], false).unwrap();
    let error = validate_truncate_references(&catalog, &targets).unwrap_err();
    assert!(
        matches!(error, SQLError::TypeMismatch(message) if message == "cannot truncate `a` because `external` references it; truncate both tables or use CASCADE")
    );
    catalog.reference_error = Some("a".into());
    let error = truncate_dependency_order(&catalog, &targets).unwrap_err();
    assert!(
        matches!(error, SQLError::Internal(message) if message == "read foreign keys: catalog read failed")
    );
}
