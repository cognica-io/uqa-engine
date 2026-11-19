//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Concrete type membership is acyclic; historical dropped attributes are not live members.

use super::{inline, invalid, retire_entries, ColumnType, SQLError, TypeReferences, Work};
use uqa_core::memory::{BudgetedMap, BudgetedVec};

struct Edge {
    child: u32,
    next: Option<usize>,
}

/// Direct edges in the native catalog's concrete type dependency graph.
pub struct TypeMembership<'r, 'a> {
    types: &'r TypeReferences<'a>,
    degree: BudgetedMap<u32, usize>,
    heads: BudgetedMap<u32, usize>,
    edges: BudgetedVec<Edge>,
}

impl<'a> TypeReferences<'a> {
    pub fn membership(&self) -> TypeMembership<'_, 'a> {
        TypeMembership {
            types: self,
            degree: BudgetedMap::new(self.memory),
            heads: BudgetedMap::new(self.memory),
            edges: BudgetedVec::new(self.memory),
        }
    }

    /// Check direct live members of domains, ranges, composites and relation row types.
    pub fn validate_membership<'t>(
        &self,
        members: impl Iterator<Item = (u32, &'t ColumnType)>,
    ) -> Result<(), SQLError> {
        let mut graph = self.membership();
        for (owner, ty) in members {
            inline(graph.member(owner, ty, &mut ()))?;
        }
        inline(graph.finish(&mut ()))
    }
}

impl TypeMembership<'_, '_> {
    /// Resolve the physical endpoint before admitting an edge. Array wrappers preserve the element dependency.
    pub async fn member(
        &mut self,
        owner: u32,
        ty: &ColumnType,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        self.types.checkpoint(work).await?;
        if !self.types.claims.contains_key(&owner) {
            return Err(invalid("missing type membership owner"));
        }
        self.types.column_type_with_work(ty, work).await?;
        let Some(child) = self
            .types
            .dependency(ty, work)
            .await?
            .filter(|oid| self.types.claims.contains_key(oid))
        else {
            return Ok(());
        };
        let count = self.degree.get(&child).copied().unwrap_or(0);
        self.degree.insert(
            child,
            count
                .checked_add(1)
                .ok_or_else(|| invalid("type membership count"))?,
        )?;
        if self.degree.get(&owner).is_none() {
            self.degree.insert(owner, 0)?;
        }
        let index = self.edges.len();
        self.edges.push(Edge {
            child,
            next: self.heads.get(&owner).copied(),
        })?;
        self.heads.insert(owner, index)?;
        Ok(())
    }

    /// Check the graph for cycles and retire the admitted scratch.
    pub async fn finish(mut self, work: &mut (impl Work + Send)) -> Result<(), SQLError> {
        let mut pending = BudgetedVec::new(self.types.memory);
        for (oid, count) in &self.degree {
            self.types.checkpoint(work).await?;
            if *count == 0 {
                pending.push(*oid)?;
            }
        }
        let mut remaining = self.degree.len();
        while let Some(owner) = pending.pop() {
            self.types.checkpoint(work).await?;
            remaining -= 1;
            let mut next = self.heads.get(&owner).copied();
            while let Some(index) = next {
                self.types.checkpoint(work).await?;
                let edge = &self.edges[index];
                let count = self
                    .degree
                    .get_mut(&edge.child)
                    .expect("admitted type child");
                *count -= 1;
                if *count == 0 {
                    pending.push(edge.child)?;
                }
                next = edge.next;
            }
        }
        if remaining != 0 {
            return Err(invalid("cyclic concrete type membership"));
        }
        let control = self.types.control();
        retire_entries(self.degree.into_entries(), &control, work).await?;
        retire_entries(self.heads.into_entries(), &control, work).await
    }
}

impl TypeReferences<'_> {
    async fn dependency(
        &self,
        mut ty: &ColumnType,
        work: &mut (impl Work + Send),
    ) -> Result<Option<u32>, SQLError> {
        while let ColumnType::Array(element) = ty {
            self.checkpoint(work).await?;
            ty = element;
        }
        Ok(match ty {
            ColumnType::Domain { oid, .. } => Some(*oid),
            ColumnType::Enum(reference) => Some(reference.oid),
            ColumnType::Composite(reference) => Some(reference.oid),
            ColumnType::DeclaredRange(reference) | ColumnType::DeclaredMultirange(reference) => {
                Some(reference.oid)
            }
            _ => None,
        })
    }
}
