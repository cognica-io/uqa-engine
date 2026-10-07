//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `deleteObjectsInList`: the targets of a deletion resolved to catalog objects before any is removed, in the search's order. An object that another target's removal removes is left to it, and that removal is ordered as the object's own would be.

use crate::catalog::projection::{CatalogDependencies, CatalogObject, RelationKind};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::dependencies::{
    DeletionFlags, DeletionTarget, DeletionTargets, DependencyGraph, DependencyKind, ObjectAddress,
};
use uqa_sql::SQLError;

pub(super) struct DeletionPlan {
    /// The objects each removal step removes, in removal order.
    pub(super) steps: Vec<CatalogObject>,
    /// Every object the deletion removes, including those another step removes.
    objects: Vec<CatalogObject>,
}

/// What a deletion removes whole, which removes the objects that belong to it.
struct Removed {
    relations: BTreeSet<RelationIdentity>,
    columns: BTreeSet<ObjectAddress>,
    types: BTreeSet<u32>,
}

impl DeletionPlan {
    pub(super) fn new(
        dependencies: &CatalogDependencies,
        targets: &DeletionTargets,
    ) -> Result<Self, SQLError> {
        let resolved = targets
            .targets()
            .iter()
            .map(|target| {
                dependencies
                    .catalog_object(target.object)
                    .map(|object| (target, object))
                    .ok_or_else(|| missing(target.object))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut removed = Removed {
            relations: BTreeSet::new(),
            columns: BTreeSet::new(),
            types: BTreeSet::new(),
        };
        for (target, object) in &resolved {
            match object {
                CatalogObject::Relation { identity, .. } => {
                    removed.relations.insert(identity.clone());
                }
                CatalogObject::Column { .. } => {
                    removed.columns.insert(target.object);
                }
                CatalogObject::Type(oid) => {
                    removed.types.insert(*oid);
                }
                _ => {}
            }
        }
        let with_another = resolved
            .iter()
            .map(|(target, object)| {
                removed_with_another_target(dependencies, target, object, &removed)
            })
            .collect::<Vec<_>>();
        let order = removal_order(dependencies.graph(), &resolved, &with_another);
        let objects = resolved
            .into_iter()
            .map(|(_, object)| object)
            .collect::<Vec<_>>();
        let steps = order
            .into_iter()
            .map(|index| objects[index].clone())
            .collect();
        Ok(Self { steps, objects })
    }

    /// The relations the plan removes or changes, which it must hold exclusively: an index is changed through its table.
    pub(super) fn relations(&self) -> BTreeSet<RelationIdentity> {
        self.objects
            .iter()
            .filter_map(|object| match object {
                CatalogObject::Relation {
                    kind: RelationKind::Index,
                    table,
                    ..
                } => table.clone(),
                CatalogObject::Relation { identity, .. } => Some(identity.clone()),
                CatalogObject::Column { relation, .. }
                | CatalogObject::RelationConstraint { relation, .. }
                | CatalogObject::ColumnDefault { relation, .. }
                | CatalogObject::Rule { relation, .. }
                | CatalogObject::Trigger { relation, .. } => Some(relation.clone()),
                CatalogObject::Type(_)
                | CatalogObject::ArrayType { .. }
                | CatalogObject::RowType { .. }
                | CatalogObject::DomainConstraint { .. }
                | CatalogObject::Routine { .. }
                | CatalogObject::Schema(_)
                | CatalogObject::ForeignWrapper { .. }
                | CatalogObject::ForeignServer { .. } => None,
            })
            .collect()
    }
}

/// The positions of the removal steps in the order they run. `deleteObjectsInList` removes every target in the search's order, which puts each object before what it depends on; removing an object with another target moves its removal to that target's step, so the steps are ordered by the dependencies of every object they remove, and otherwise by the earliest position of those objects.
fn removal_order(
    graph: &DependencyGraph,
    resolved: &[(&DeletionTarget, CatalogObject)],
    with_another: &[bool],
) -> Vec<usize> {
    let positions = resolved
        .iter()
        .enumerate()
        .map(|(index, (target, _))| (target.object, index))
        .collect::<BTreeMap<_, _>>();
    // A column of a relation removed whole is removed with the relation.
    let position = |address: ObjectAddress| {
        positions.get(&address).copied().or_else(|| {
            (address.sub_id != 0)
                .then(|| {
                    positions
                        .get(&ObjectAddress::whole(address.class_id, address.object_id))
                        .copied()
                })
                .flatten()
        })
    };
    let owners = resolved
        .iter()
        .enumerate()
        .map(|(index, (target, _))| {
            if with_another[index] {
                owner(graph, target.object, &position).filter(|owner| *owner != index)
            } else {
                Some(index)
            }
        })
        .collect::<Vec<_>>();
    let step = |index: usize| {
        let mut current = index;
        for _ in 0..resolved.len() {
            let owner = owners[current]?;
            if owner == current {
                return Some(current);
            }
            current = owner;
        }
        None
    };
    let mut earliest = BTreeMap::new();
    for index in 0..resolved.len() {
        if let Some(step) = step(index) {
            let entry = earliest.entry(step).or_insert(step);
            *entry = (*entry).min(index);
        }
    }
    let mut successors: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    let mut predecessors = earliest
        .keys()
        .map(|step| (*step, 0usize))
        .collect::<BTreeMap<_, _>>();
    for (index, (target, _)) in resolved.iter().enumerate() {
        let Some(from) = step(index) else {
            continue;
        };
        for edge in graph.references_of(target.object) {
            let Some(to) = position(edge.referenced).and_then(step) else {
                continue;
            };
            if from != to && successors.entry(from).or_default().insert(to) {
                *predecessors.entry(to).or_default() += 1;
            }
        }
    }
    let mut ready = predecessors
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(step, _)| (earliest[step], *step))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(earliest.len());
    let mut ordered = BTreeSet::new();
    while order.len() < earliest.len() {
        // A dependency cycle leaves nothing ready: its earliest step goes first, as the search's order would have it.
        let next = ready.pop_first().or_else(|| {
            predecessors
                .iter()
                .filter(|(step, _)| !ordered.contains(*step))
                .map(|(step, _)| (earliest[step], *step))
                .min()
        });
        let Some((_, step)) = next else {
            break;
        };
        if !ordered.insert(step) {
            continue;
        }
        predecessors.insert(step, 0);
        order.push(step);
        for successor in successors.get(&step).into_iter().flatten() {
            let count = predecessors.entry(*successor).or_default();
            if *count > 0 {
                *count -= 1;
                if *count == 0 {
                    ready.insert((earliest[successor], *successor));
                }
            }
        }
    }
    order
}

/// The position of the target whose removal removes `object`: the object it is part of, the partitioned parent object it belongs to, or the column or relation it goes with automatically.
fn owner(
    graph: &DependencyGraph,
    object: ObjectAddress,
    position: &impl Fn(ObjectAddress) -> Option<usize>,
) -> Option<usize> {
    if object.sub_id != 0 {
        return position(ObjectAddress::whole(object.class_id, object.object_id));
    }
    [
        DependencyKind::Internal,
        DependencyKind::PartitionPrimary,
        DependencyKind::Auto,
    ]
    .into_iter()
    .find_map(|kind| {
        graph
            .references_of(object)
            .filter(|edge| edge.kind == kind && edge.dependent == object)
            .find_map(|edge| position(edge.referenced))
    })
}

/// Whether removing another target removes this object, which is then left to it: array and row types go with their element types and relations, a view's `_RETURN` rule with the view, an index that implements a constraint with the constraint, a generation expression with its column, a partition's index or constraint with its parent's, a `NOT NULL` constraint with its column or relation, a domain constraint with its domain, and a column with its relation. Every other object is removed on its own in the search's order, as `deleteObjectsInList` does, so nothing that remains refers to an object already removed; that includes the indexes, constraints, defaults, triggers and rules of a relation removed whole, and the identity sequence of a removed column, which is a relation of its own.
fn removed_with_another_target(
    dependencies: &CatalogDependencies,
    target: &DeletionTarget,
    object: &CatalogObject,
    removed: &Removed,
) -> bool {
    let internal = target.flags.contains(DeletionFlags::INTERNAL);
    let partition = target.flags.contains(DeletionFlags::PARTITION);
    match object {
        CatalogObject::ArrayType { .. } | CatalogObject::RowType { .. } => true,
        CatalogObject::Rule { name, .. } => name == "_RETURN",
        CatalogObject::Relation {
            kind: RelationKind::Index,
            ..
        } => internal || partition,
        CatalogObject::ColumnDefault { .. } => internal,
        CatalogObject::RelationConstraint {
            relation, not_null, ..
        } => {
            partition
                || (*not_null
                    && (removed.relations.contains(relation)
                        || dependencies
                            .graph()
                            .references_of(target.object)
                            .any(|edge| {
                                edge.kind == DependencyKind::Auto
                                    && removed.columns.contains(&edge.referenced)
                            })))
        }
        CatalogObject::Column { relation, .. } => removed.relations.contains(relation),
        CatalogObject::DomainConstraint { domain, .. } => removed.types.contains(domain),
        CatalogObject::Relation { .. }
        | CatalogObject::Type(_)
        | CatalogObject::Trigger { .. }
        | CatalogObject::Routine { .. }
        | CatalogObject::Schema(_)
        | CatalogObject::ForeignWrapper { .. }
        | CatalogObject::ForeignServer { .. } => false,
    }
}

fn missing(object: ObjectAddress) -> SQLError {
    SQLError::Internal(format!(
        "deletion target {} of class {} column {} is not a catalog object",
        object.object_id, object.class_id, object.sub_id
    ))
}
