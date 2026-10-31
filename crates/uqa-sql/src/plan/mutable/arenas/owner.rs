//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar-query occurrence copies retain independent child plans and admit the replacement arena before moving any existing child.

use super::super::{Copying, MutableQueryPlan, OwnedScalarExpr, RewriteMemory};
use crate::{plan::QueryPlan, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

/// A private scalar-query namespace. Its syntax and constructor records move together; observers can borrow children while only owning operations can add them.
pub struct ScalarQueryArena {
    pub(in crate::plan::mutable) queries: Vec<QueryPlan>,
    pub(in crate::plan::mutable) memory: Option<RewriteMemory>,
}

impl ScalarQueryArena {
    pub(in crate::plan) fn from_rewrite_parts(
        queries: Vec<QueryPlan>,
        memory: Option<RewriteMemory>,
    ) -> Self {
        Self { queries, memory }
    }

    pub(crate) fn copy_from(
        queries: &[QueryPlan],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let Some(mut copying) = Copying::for_rewrite(*control) else {
            return Ok(Self {
                queries: queries.to_vec(),
                memory: None,
            });
        };
        let queries = copying.map(queries.iter(), |copying, query| copying.query(query))?;
        Ok(Self {
            queries,
            memory: Some(copying.into_rewrite_memory()),
        })
    }

    pub(crate) fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<QueryPlan>>, SQLError> {
        control.check()?;
        Ok(control.finish(
            self.queries,
            self.memory.map(RewriteMemory::into_reservation),
        )?)
    }

    pub fn map_query(
        &mut self,
        index: usize,
        control: &ProductionControl<'_>,
        map: impl FnOnce(MutableQueryPlan) -> Result<MutableQueryPlan, SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let query = self.take(index);
        let (query, owner) = map(query)?.into_rewrite_parts(control)?;
        self.queries[index] = query;
        if let Some(memory) = &mut self.memory {
            *memory
                .item_mut(index)
                .as_mut()
                .expect("arena query item")
                .field_mut("query") = owner;
        }
        Ok(())
    }

    pub(crate) fn append_arena(
        &mut self,
        mut other: Self,
        control: &ProductionControl<'_>,
    ) -> Result<usize, SQLError> {
        control.check()?;
        let previous = self.queries.len();
        if other.queries.is_empty() {
            return Ok(previous);
        }
        let total = previous + other.queries.len();
        let mut take = |index| {
            if index < previous {
                self.take(index)
            } else {
                other.take(index - previous)
            }
        };
        let (queries, memory) = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let queries =
                copying.map(0..total, |copying, index| copying.take_query(take(index)))?;
            (queries, Some(copying.into_rewrite_memory()))
        } else {
            let mut queries = Vec::with_capacity(total);
            for index in 0..total {
                control.check()?;
                queries.push(take(index).into_rewrite_parts(control)?.0);
            }
            (queries, None)
        };
        self.queries = queries;
        self.memory = memory;
        Ok(previous)
    }

    /// Add an owning expression to this namespace, moving its query children exactly once and preserving their private nested arenas.
    pub fn append_expression(
        &mut self,
        expression: super::super::MutableExpressionPlan,
        control: &ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let (scalar, arena) = expression.into_parts(control)?;
        let offset = self.append_arena(arena, control)?;
        scalar.remap_subquery_ids(control, &mut |id| id + offset)
    }

    pub fn queries(&self) -> &[QueryPlan] {
        &self.queries
    }

    /// Give every scalar-query occurrence a new result-cache identity. Even repeated references to one source slot are copied independently, in the existing child-before-parent scalar order.
    pub fn copy_subquery_occurrences(
        &mut self,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        control.check()?;
        let mut copies = ProductionVec::new(*control);
        let original_len = self.queries.len();
        let expression = expression.remap_query_occurrences(control, &mut |id| {
            let query = self.queries.get(id).ok_or_else(|| {
                SQLError::Internal("copied scalar subquery is outside its owning arena".into())
            })?;
            let query = query.copy_for_rewrite(control)?;
            let next = original_len + copies.len();
            copies.push_produced(control.finish(query, control.empty_reservation())?)?;
            Ok(next)
        })?;
        if !copies.is_empty() {
            self.append(copies.finish()?, control)?;
        }
        Ok(expression)
    }

    /// Append independently owned query children, retaining their original nested namespaces. The produced outer vector owns only the staging buffer; every child owns its own payload.
    pub(crate) fn append(
        &mut self,
        queries: Produced<Vec<MutableQueryPlan>>,
        control: &ProductionControl<'_>,
    ) -> Result<usize, SQLError> {
        control.check()?;
        let original_len = self.queries.len();
        let count = original_len + queries.len();
        let (queries, buffer) = queries.into_parts();
        let mut pending = Pending {
            queries: queries.into_iter(),
            _buffer: buffer,
        };
        let mut next = |index| {
            if index < original_len {
                self.take(index)
            } else {
                pending.queries.next().expect("appended arena child")
            }
        };
        let (queries, memory) = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let queries =
                copying.map(0..count, |copying, index| copying.take_query(next(index)))?;
            (queries, Some(copying.into_rewrite_memory()))
        } else {
            let mut queries = Vec::with_capacity(count);
            for index in 0..count {
                control.check()?;
                queries.push(next(index).into_rewrite_parts(control)?.0);
            }
            (queries, None)
        };
        self.queries = queries;
        self.memory = memory;
        control.check()?;
        Ok(original_len)
    }

    fn take(&mut self, index: usize) -> MutableQueryPlan {
        let query = std::mem::replace(
            &mut self.queries[index],
            super::super::queries::empty_query(),
        );
        let memory = self.memory.as_mut().and_then(|memory| {
            memory
                .item_mut(index)
                .as_mut()
                .expect("arena query item")
                .field_mut("query")
                .take()
        });
        MutableQueryPlan::from_rewrite_parts(query, memory)
    }
}

struct Pending {
    queries: std::vec::IntoIter<MutableQueryPlan>,
    _buffer: Option<uqa_core::memory::MemoryReservation>,
}
