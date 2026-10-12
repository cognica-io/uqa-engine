//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Order catalog metadata upgrades through the stored queries' actual relation dependencies.

use super::{BTreeMap, BTreeSet, RelationIdentity, StoredView};
use crate::SQLError;

/// Visit only the target definitions and their view dependencies, collecting each query once; intermediate ordinary views can connect two materialized views requiring migration.
pub fn view_metadata_dependency_order(
    views: &BTreeMap<RelationIdentity, StoredView>,
    targets: &BTreeSet<RelationIdentity>,
) -> Result<Vec<RelationIdentity>, SQLError> {
    let mut remaining = BTreeMap::new();
    let mut dependents: BTreeMap<RelationIdentity, Vec<RelationIdentity>> = BTreeMap::new();
    let mut pending = targets.iter().cloned().collect::<Vec<_>>();
    while let Some(relation) = pending.pop() {
        if remaining.contains_key(&relation) {
            continue;
        }
        let view = views.get(&relation).ok_or_else(|| {
            SQLError::Internal("view disappeared during metadata dependency analysis".into())
        })?;
        let mut dependencies = crate::catalog::events::RuleDependencies::default();
        crate::catalog::events::dependencies::collect_query_relation_dependencies(
            &view.query,
            &mut dependencies,
            &BTreeSet::new(),
        )?;
        let mut count = 0usize;
        for dependency in dependencies.relations {
            if views.contains_key(&dependency) {
                dependents
                    .entry(dependency.clone())
                    .or_default()
                    .push(relation.clone());
                pending.push(dependency);
                count += 1;
            }
        }
        remaining.insert(relation, count);
    }
    let mut ready = remaining
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(relation, _)| relation.clone())
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(targets.len());
    let mut visited = 0;
    while let Some(relation) = ready.pop_first() {
        visited += 1;
        if targets.contains(&relation) {
            ordered.push(relation.clone());
        }
        for dependent in dependents.remove(&relation).unwrap_or_default() {
            let count = remaining
                .get_mut(&dependent)
                .expect("registered view dependency");
            *count -= 1;
            if *count == 0 {
                ready.insert(dependent);
            }
        }
    }
    if visited != remaining.len() {
        return Err(SQLError::Internal(
            "stored view metadata has a recursive dependency".into(),
        ));
    }
    Ok(ordered)
}
