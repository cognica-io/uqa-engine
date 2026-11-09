//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Concrete type membership is acyclic; historical dropped attributes are not live members.

use super::{invalid, ColumnType, SQLError, TypeReferences};
use uqa_core::memory::{BudgetedMap, BudgetedVec};

struct Edge {
    child: u32,
    next: Option<usize>,
}

impl TypeReferences<'_> {
    /// Check direct live members of domains, ranges, composites and relation row types. Array wrappers preserve the element dependency. Every endpoint is checked before the graph can admit it.
    pub fn validate_membership<'a>(
        &self,
        members: impl Iterator<Item = (u32, &'a ColumnType)>,
    ) -> Result<(), SQLError> {
        let mut degree = BudgetedMap::new(self.memory);
        let mut heads = BudgetedMap::new(self.memory);
        let mut edges = BudgetedVec::new(self.memory);
        for (owner, ty) in members {
            self.cancellation.check()?;
            if !self.claims.contains_key(&owner) {
                return Err(invalid("missing type membership owner"));
            }
            self.column_type(ty)?;
            let Some(child) = dependency(ty).filter(|oid| self.claims.contains_key(oid)) else {
                continue;
            };
            let count = degree.get(&child).copied().unwrap_or(0usize);
            degree.insert(
                child,
                count
                    .checked_add(1)
                    .ok_or_else(|| invalid("type membership count"))?,
            )?;
            if degree.get(&owner).is_none() {
                degree.insert(owner, 0)?;
            }
            let index = edges.len();
            edges.push(Edge {
                child,
                next: heads.get(&owner).copied(),
            })?;
            heads.insert(owner, index)?;
        }
        let mut pending = BudgetedVec::new(self.memory);
        for (oid, count) in &degree {
            self.cancellation.check()?;
            if *count == 0 {
                pending.push(*oid)?;
            }
        }
        let mut remaining = degree.len();
        while let Some(owner) = pending.pop() {
            self.cancellation.check()?;
            remaining -= 1;
            let mut next = heads.get(&owner).copied();
            while let Some(index) = next {
                self.cancellation.check()?;
                let edge = &edges[index];
                let count = degree.get_mut(&edge.child).expect("admitted type child");
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
        Ok(())
    }
}

fn dependency(mut ty: &ColumnType) -> Option<u32> {
    while let ColumnType::Array(element) = ty {
        ty = element;
    }
    match ty {
        ColumnType::Domain { oid, .. } => Some(*oid),
        ColumnType::Enum(reference) => Some(reference.oid),
        ColumnType::Composite(reference) => Some(reference.oid),
        ColumnType::DeclaredRange(reference) | ColumnType::DeclaredMultirange(reference) => {
            Some(reference.oid)
        }
        _ => None,
    }
}
