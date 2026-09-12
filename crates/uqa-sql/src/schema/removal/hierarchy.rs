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
