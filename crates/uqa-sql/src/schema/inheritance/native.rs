//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed native inheritance references must form a complete acyclic relation graph.

use crate::{
    ast::{PartitionBound, PartitionStrategy, RelationPersistence, TableHierarchy},
    catalog::{
        resolution::bound_relation_identity_with_work,
        work::{inline, retire_entries, ReferenceWork},
        CatalogWork,
    },
    SQLError,
};
use uqa_core::{
    memory::{Budgeted, BudgetedMap, BudgetedVec, MemoryBudget, OwnedMap, ProductionControl},
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
    inline(validate_with_work(tables, memory, cancellation, &mut ()))
}

/// Validate the same ordered declarations, parent edges and acyclicity graph within the caller's work windows.
pub async fn validate_with_work<'a, W: CatalogWork + Send>(
    tables: impl Iterator<
        Item = (
            &'a RelationIdentity,
            &'a TableHierarchy,
            RelationPersistence,
        ),
    >,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut W,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    control.check()?;
    let mut input = ReferenceWork {
        control: &control,
        work,
    };
    let mut relations = OwnedMap::new();
    let mut nodes = BudgetedVec::new(memory);
    for (name, hierarchy, persistence) in tables {
        input.checkpoint().await?;
        let index = nodes.len();
        nodes.push(Node::default())?;
        let key = Budgeted::new(name, memory.reserve(0)?);
        if relations
            .insert_admitted_key_from(
                key,
                Relation {
                    hierarchy,
                    persistence,
                    index,
                },
                &mut input,
            )
            .await?
            .is_some()
        {
            return Err(invalid("duplicate relation"));
        }
    }
    let mut edges = BudgetedVec::new(memory);
    for (_, relation) in &relations {
        input.checkpoint().await?;
        parents(
            relation, &relations, &mut nodes, &mut edges, memory, &mut input,
        )
        .await?;
    }
    acyclic(&mut nodes, &edges, memory, &mut input).await?;
    retire_entries(relations.into_iter(), &control, work).await
}

async fn parents(
    relation: &Relation<'_>,
    relations: &OwnedMap<&RelationIdentity, Relation<'_>>,
    nodes: &mut [Node],
    edges: &mut BudgetedVec<Edge>,
    memory: &MemoryBudget,
    input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
) -> Result<(), SQLError> {
    shape(relation.hierarchy, memory, input).await?;
    let mut selected = BudgetedMap::new(memory);
    for parent in &relation.hierarchy.parents {
        input.checkpoint().await?;
        let name = bound_relation_identity_with_work(parent, input.control, input.work).await?;
        let target = relations
            .get_from(&*name, input)
            .await?
            .ok_or_else(|| invalid("missing parent table"))?;
        if selected.insert(target.index, ())?.is_some() {
            return Err(invalid("repeated parent table"));
        }
        parent_shape(relation.hierarchy, target.hierarchy)?;
        super::check_parent_persistence_with_work(
            parent,
            target.persistence,
            relation.persistence,
            relation.hierarchy.is_partition(),
            input.control,
            input.work,
        )
        .await?;
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
    retire_entries(selected.into_entries(), input.control, input.work).await
}

async fn shape(
    hierarchy: &TableHierarchy,
    memory: &MemoryBudget,
    input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
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
        input.checkpoint().await?;
        if *number <= 0 || selected.insert(*number, ())?.is_some() {
            return Err(invalid("parent sequence identity"));
        }
    }
    retire_entries(selected.into_entries(), input.control, input.work).await
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

async fn acyclic(
    nodes: &mut [Node],
    edges: &[Edge],
    memory: &MemoryBudget,
    input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
) -> Result<(), SQLError> {
    let mut ready = BudgetedVec::new(memory);
    for (index, node) in nodes.iter().enumerate() {
        input.checkpoint().await?;
        if node.incoming == 0 {
            ready.push(index)?;
        }
    }
    let mut remaining = nodes.len();
    while let Some(index) = ready.pop() {
        input.checkpoint().await?;
        remaining -= 1;
        let mut edge = nodes[index].first_parent;
        while let Some(index) = edge {
            input.checkpoint().await?;
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
