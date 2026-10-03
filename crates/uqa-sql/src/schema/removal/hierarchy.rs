//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expand DROP targets through retained inheritance and partition metadata.
use crate::ast::TableHierarchy;
use std::{collections::BTreeSet, ops::Deref};
use uqa_core::RelationIdentity;
pub type HierarchyDropRead<'a> = Box<dyn Deref<Target = TableHierarchy> + 'a>;
pub trait HierarchyDropTable {
    fn hierarchy(&self) -> HierarchyDropRead<'_>;
}
pub type HierarchyDropEntries<'a> =
    Box<dyn Iterator<Item = (&'a RelationIdentity, &'a dyn HierarchyDropTable)> + 'a>;
pub trait HierarchyDropTables {
    fn iter(&self) -> HierarchyDropEntries<'_>;
}
pub trait HierarchyDropCatalog {
    fn tables(&self) -> Box<dyn HierarchyDropTables + '_>;
}
pub fn hierarchy_drop_targets(
    catalog: &dyn HierarchyDropCatalog,
    roots: &[String],
    cascade: bool,
) -> (Vec<String>, Vec<String>) {
    let mut targets = roots.iter().cloned().collect::<BTreeSet<_>>();
    let mut blockers = BTreeSet::new();
    loop {
        let mut added = false;
        let tables = catalog.tables();
        for (identity, table) in tables.iter() {
            let candidate = identity.qualified_name();
            if targets.contains(&candidate) {
                continue;
            }
            let hierarchy = table.hierarchy();
            if !hierarchy
                .parents
                .iter()
                .any(|parent| targets.contains(parent))
            {
                continue;
            }
            if hierarchy.is_partition() || cascade {
                added |= targets.insert(candidate);
            } else {
                blockers.insert(candidate);
            }
        }
        if !added {
            break;
        }
    }
    (
        targets.into_iter().collect(),
        blockers.into_iter().collect(),
    )
}

#[cfg(test)]
mod tests;

/// The partitioned tables outside `targets` that a dropped partition is a partition of, nearest first. Their rows include the dropped partition's rows, so a foreign key that references one of them references dropped rows, as the constraint `PostgreSQL` derives on each referenced partition does.
pub fn surviving_partition_ancestors(
    catalog: &dyn HierarchyDropCatalog,
    targets: &[String],
) -> Vec<String> {
    let tables = catalog.tables();
    let parents = tables
        .iter()
        .filter_map(|(identity, table)| {
            let hierarchy = table.hierarchy();
            hierarchy
                .is_partition()
                .then(|| hierarchy.parents.first().cloned())
                .flatten()
                .map(|parent| (identity.qualified_name(), parent))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let dropped = targets.iter().collect::<BTreeSet<_>>();
    let mut ancestors = Vec::new();
    for target in targets {
        let mut visited = BTreeSet::new();
        let mut current = target;
        while let Some(parent) = parents.get(current) {
            if !visited.insert(parent) {
                break;
            }
            if !dropped.contains(parent) && !ancestors.contains(parent) {
                ancestors.push(parent.clone());
            }
            current = parent;
        }
    }
    ancestors
}
