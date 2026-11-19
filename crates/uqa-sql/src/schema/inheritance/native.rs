//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed native inheritance references must form a complete acyclic relation graph.

use crate::{
    ast::{PartitionBound, PartitionStrategy, RelationPersistence, TableHierarchy},
    catalog::resolution::bound_relation_identity_with_control,
    SQLError,
};
use uqa_core::{
    memory::{BudgetedMap, BudgetedVec, MemoryBudget, ProductionControl},
    CancellationToken, RelationIdentity,
};

struct Relation<'a> {
    hierarchy: &'a TableHierarchy,
    persistence: RelationPersistence,
    index: usize,
}

#[derive(Default)]
struct Node {
    incoming: usize,
    first_parent: Option<usize>,
}

struct Edge {
    parent: usize,
    next: Option<usize>,
}

/// Validate complete retained table membership without rebinding parents, evaluating partition expressions or cloning definitions. Empty legacy sequence-number arrays preserve their declaration-order interpretation. Workspace and every parsed parent identity use the supplied allowance; failure leaves the input unchanged.
pub fn validate<'a>(
    tables: impl Iterator<
        Item = (
            &'a RelationIdentity,
            &'a TableHierarchy,
            RelationPersistence,
        ),
    >,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    control.check()?;
    let mut relations = BudgetedMap::new(memory);
    let mut nodes = BudgetedVec::new(memory);
    for (name, hierarchy, persistence) in tables {
        control.check()?;
        let index = nodes.len();
        nodes.push(Node::default())?;
        if relations
            .insert(
                name,
                Relation {
                    hierarchy,
                    persistence,
                    index,
                },
            )?
            .is_some()
        {
            return Err(invalid("duplicate relation"));
        }
    }
    let mut edges = BudgetedVec::new(memory);
    for (_, relation) in &relations {
        control.check()?;
        shape(relation.hierarchy, memory, cancellation)?;
        let mut selected = BudgetedMap::new(memory);
        for parent in &relation.hierarchy.parents {
            control.check()?;
            let name = bound_relation_identity_with_control(parent, &control)?;
            let target = relations
                .get(&*name)
                .ok_or_else(|| invalid("missing parent table"))?;
            if selected.insert(target.index, ())?.is_some() {
                return Err(invalid("repeated parent table"));
            }
            parent_shape(relation.hierarchy, target.hierarchy)?;
            super::check_parent_persistence(
                parent,
                target.persistence,
                relation.persistence,
                relation.hierarchy.is_partition(),
            )?;
            let index = edges.len();
            edges.push(Edge {
                parent: target.index,
                next: nodes[relation.index].first_parent,
            })?;
            nodes[relation.index].first_parent = Some(index);
            nodes[target.index].incoming = nodes[target.index]
                .incoming
                .checked_add(1)
                .ok_or_else(|| invalid("parent count overflow"))?;
        }
    }
    acyclic(&mut nodes, &edges, memory, cancellation)
}

fn shape(
    hierarchy: &TableHierarchy,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    if hierarchy.is_partition() && hierarchy.parents.len() != 1 {
        return Err(invalid("partition parent cardinality"));
    }
    let numbers = &hierarchy.parent_sequence_numbers;
    if !numbers.is_empty() && numbers.len() != hierarchy.parents.len() {
        return Err(invalid("parent sequence membership"));
    }
    let mut selected = BudgetedMap::new(memory);
    for number in numbers {
        cancellation.check()?;
        if *number <= 0 || selected.insert(*number, ())?.is_some() {
            return Err(invalid("parent sequence identity"));
        }
    }
    Ok(())
}

fn parent_shape(child: &TableHierarchy, parent: &TableHierarchy) -> Result<(), SQLError> {
    let Some(bound) = &child.partition_bound else {
        return if parent.partition_spec.is_none() && !parent.is_partition() {
            Ok(())
        } else {
            Err(invalid("ordinary inheritance from a partitioned relation"))
        };
    };
    let spec = parent
        .partition_spec
        .as_ref()
        .ok_or_else(|| invalid("partition parent has no key"))?;
    if matches!(
        (spec.strategy, bound),
        (
            PartitionStrategy::List | PartitionStrategy::Range,
            PartitionBound::Default
        ) | (PartitionStrategy::List, PartitionBound::List(_))
            | (PartitionStrategy::Range, PartitionBound::Range { .. })
            | (PartitionStrategy::Hash, PartitionBound::Hash { .. })
    ) {
        Ok(())
    } else {
        Err(invalid("partition strategy disagrees with parent"))
    }
}

fn acyclic(
    nodes: &mut BudgetedVec<Node>,
    edges: &BudgetedVec<Edge>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let mut ready = BudgetedVec::new(memory);
    for (index, node) in nodes.iter().enumerate() {
        cancellation.check()?;
        if node.incoming == 0 {
            ready.push(index)?;
        }
    }
    let mut remaining = nodes.len();
    while let Some(index) = ready.pop() {
        cancellation.check()?;
        remaining -= 1;
        let mut edge = nodes[index].first_parent;
        while let Some(index) = edge {
            cancellation.check()?;
            let parent = edges[index].parent;
            nodes[parent].incoming -= 1;
            if nodes[parent].incoming == 0 {
                ready.push(parent)?;
            }
            edge = edges[index].next;
        }
    }
    if remaining != 0 {
        return Err(invalid("cyclic inheritance"));
    }
    Ok(())
}

fn invalid(reason: &str) -> SQLError {
    SQLError::Internal(format!("invalid native table hierarchy: {reason}"))
}

#[cfg(test)]
mod tests;
