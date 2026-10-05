//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The dependencies of one catalog snapshot, indexed as `pg_depend`'s two indexes are: by the depending object and by the referenced object.

use super::{Dependency, ObjectAddress};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct DependencyGraph {
    edges: Vec<Dependency>,
    by_dependent: BTreeMap<(u32, u32), Vec<usize>>,
    by_referenced: BTreeMap<(u32, u32), Vec<usize>>,
}

impl DependencyGraph {
    /// Index the dependencies in the order they were recorded. `pg_depend` keeps a dependency that two recordings of one object share, such as a routine's argument type that its body names again, and the deletion search visits the object once either way.
    pub fn new(edges: impl IntoIterator<Item = Dependency>) -> Self {
        let edges = edges.into_iter().collect::<Vec<_>>();
        let mut by_dependent: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
        let mut by_referenced: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
        for (index, edge) in edges.iter().enumerate() {
            by_dependent
                .entry((edge.dependent.class_id, edge.dependent.object_id))
                .or_default()
                .push(index);
            by_referenced
                .entry((edge.referenced.class_id, edge.referenced.object_id))
                .or_default()
                .push(index);
        }
        // `pg_depend`'s indexes order the rows of one object by column number, then as they were stored.
        for rows in by_dependent.values_mut() {
            rows.sort_by_key(|index| edges[*index].dependent.sub_id);
        }
        for rows in by_referenced.values_mut() {
            rows.sort_by_key(|index| edges[*index].referenced.sub_id);
        }
        Self {
            edges,
            by_dependent,
            by_referenced,
        }
    }

    pub fn edges(&self) -> &[Dependency] {
        &self.edges
    }

    /// What `object` depends on; for a whole object, what any of its columns depends on too.
    pub fn references_of(&self, object: ObjectAddress) -> impl Iterator<Item = &Dependency> {
        self.by_dependent
            .get(&(object.class_id, object.object_id))
            .into_iter()
            .flatten()
            .map(|index| &self.edges[*index])
            .filter(move |edge| object.sub_id == 0 || edge.dependent.sub_id == object.sub_id)
    }

    /// What depends on `object`; for a whole object, what depends on any of its columns too.
    pub fn dependents_of(&self, object: ObjectAddress) -> impl Iterator<Item = &Dependency> {
        self.by_referenced
            .get(&(object.class_id, object.object_id))
            .into_iter()
            .flatten()
            .map(|index| &self.edges[*index])
            .filter(move |edge| object.sub_id == 0 || edge.referenced.sub_id == object.sub_id)
    }
}
